//! Thread roles, dedicated threads, the IO pool preset and moving work between pools (milestone T5), seen from
//! outside the crate.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use threading::{
    DEFAULT_IO_WORKERS, DedicatedThread, JobPriority, PoolConfig, StopToken, ThreadConfig,
    ThreadPool, ThreadPriority, ThreadRole, ThreadingError, WorkerCount, assert_main_thread,
    is_main_thread,
};

/// How long a test waits for something that should happen at once before it calls it a hang.
const PATIENCE: Duration = Duration::from_secs(60);

fn pool_with(workers: usize) -> ThreadPool {
    ThreadPool::new(PoolConfig::new().with_workers(WorkerCount::Exact(workers)))
        .expect("the pool starts")
}

/// Polls `condition` until it is true, or fails the test after `PATIENCE`.
fn eventually(what: &str, condition: impl Fn() -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for: {what}");
        thread::sleep(Duration::from_millis(1));
    }
}

mod roles {
    use super::*;

    /// Runs `job` as a job of `pool` and returns what it answers, without letting the calling thread run it
    /// (a thread that waits for a job may help and run it itself, which would make it say its own role).
    fn answer_on_a_worker<T: Send + 'static>(
        pool: &ThreadPool,
        job: impl FnOnce() -> T + Send + 'static,
    ) -> T {
        let (sender, receiver) = mpsc::channel();
        drop(pool.spawn(move || sender.send(job()).expect("the test is listening")));
        receiver.recv_timeout(PATIENCE).expect("a worker answers")
    }

    #[test]
    fn the_workers_of_a_default_pool_have_the_worker_role() {
        let pool = pool_with(2);
        assert_eq!(pool.role(), ThreadRole::Worker);
        assert_eq!(
            answer_on_a_worker(&pool, ThreadRole::current),
            ThreadRole::Worker
        );
    }

    #[test]
    fn a_pool_can_give_its_workers_another_role() {
        let pool = ThreadPool::new(
            PoolConfig::new()
                .with_workers(WorkerCount::Exact(1))
                .with_role(ThreadRole::Custom("physics")),
        )
        .unwrap();
        assert_eq!(pool.role(), ThreadRole::Custom("physics"));
        assert_eq!(
            answer_on_a_worker(&pool, ThreadRole::current),
            ThreadRole::Custom("physics")
        );
    }

    #[test]
    fn the_roles_of_two_pools_do_not_mix() {
        let compute = pool_with(1);
        let io = ThreadPool::new(PoolConfig::io()).unwrap();
        assert_eq!(
            answer_on_a_worker(&compute, ThreadRole::current),
            ThreadRole::Worker
        );
        assert_eq!(answer_on_a_worker(&io, ThreadRole::current), ThreadRole::Io);
        assert_eq!(ThreadRole::current(), ThreadRole::Unknown);
    }

    #[test]
    fn the_calling_thread_keeps_its_role_when_it_helps_a_pool() {
        // No workers: the job runs on the thread that waits for it, whatever that thread is.
        let pool = pool_with(0);
        let _main = ThreadRole::Main.enter();
        assert_eq!(pool.spawn(ThreadRole::current).wait(), ThreadRole::Main);
    }

    #[test]
    fn code_restricted_to_the_main_thread_can_check_and_report() {
        fn main_only() -> Result<&'static str, ThreadingError> {
            ThreadRole::Main.require()?;
            Ok("window created")
        }

        let pool = pool_with(1);
        let on_worker = answer_on_a_worker(&pool, main_only);
        match on_worker {
            Err(ThreadingError::WrongThread { expected, actual }) => {
                assert_eq!(expected, ThreadRole::Main);
                assert_eq!(actual, ThreadRole::Worker);
            }
            other => panic!("unexpected result: {other:?}"),
        }

        let _main = ThreadRole::Main.enter();
        assert_eq!(main_only().unwrap(), "window created");
    }

    #[test]
    fn a_job_that_asserts_the_main_thread_fails_on_a_worker_and_the_pool_survives() {
        let pool = pool_with(1);
        let (sender, receiver) = mpsc::channel();
        let failing = pool.spawn(move || {
            sender.send(()).unwrap();
            assert_main_thread();
        });
        receiver.recv_timeout(PATIENCE).unwrap();
        let failure = failing.wait_catch().expect_err("the assertion fired");
        let message = failure.message().expect("a formatted message");
        assert!(message.contains("main thread"), "{message}");
        assert!(message.contains("worker thread"), "{message}");
        assert!(
            message.contains("cv-worker-0"),
            "the thread's name: {message}"
        );
        assert_eq!(pool.spawn(|| 5).wait(), 5);
    }

    #[test]
    fn a_blocking_call_can_refuse_the_main_thread() {
        fn read_file_blocking() {
            ThreadRole::Main.assert_not_current();
            ThreadRole::Render.assert_not_current();
        }

        let io = ThreadPool::new(PoolConfig::io()).unwrap();
        answer_on_a_worker(&io, read_file_blocking);

        let _main = ThreadRole::Main.enter();
        assert!(is_main_thread());
        assert!(std::panic::catch_unwind(read_file_blocking).is_err());
    }
}

mod io_pool {
    use super::*;

    #[test]
    fn the_preset_starts_two_named_io_workers() {
        let io = ThreadPool::new(PoolConfig::io()).unwrap();
        assert_eq!(io.worker_count(), DEFAULT_IO_WORKERS);
        assert_eq!(io.role(), ThreadRole::Io);
        let (sender, receiver) = mpsc::channel();
        drop(io.spawn(move || {
            let name = thread::current().name().map(str::to_owned);
            sender.send(name).unwrap();
        }));
        let name = receiver
            .recv_timeout(PATIENCE)
            .unwrap()
            .expect("a named thread");
        assert!(name.starts_with("cv-io-"), "{name}");
    }

    #[test]
    fn blocking_io_jobs_do_not_keep_the_compute_pool_from_working() {
        let io = ThreadPool::new(PoolConfig::io()).unwrap();
        let compute = pool_with(1);

        // Both IO workers are busy until the test lets them go.
        let (release, gate) = mpsc::channel::<()>();
        let gate = Arc::new(std::sync::Mutex::new(gate));
        let (started, running) = mpsc::channel::<()>();
        let blocked: Vec<_> = (0..DEFAULT_IO_WORKERS)
            .map(|_| {
                let gate = Arc::clone(&gate);
                let started = started.clone();
                io.spawn(move || {
                    started.send(()).unwrap();
                    // One job at a time waits for the single release message; the loop below sends enough.
                    gate.lock()
                        .unwrap()
                        .recv_timeout(PATIENCE)
                        .expect("the test releases the IO jobs");
                })
            })
            .collect();
        for _ in 0..DEFAULT_IO_WORKERS {
            running
                .recv_timeout(PATIENCE)
                .expect("an IO worker started");
        }

        // The compute pool answers although every IO worker is blocked.
        assert_eq!(compute.spawn(|| 6 * 7).wait(), 42);

        for _ in 0..DEFAULT_IO_WORKERS {
            release.send(()).unwrap();
        }
        for handle in blocked {
            handle.wait();
        }
    }
}

mod hopping_between_pools {
    use super::*;

    #[test]
    fn a_continuation_runs_on_the_other_pool() {
        let io = ThreadPool::new(PoolConfig::io()).unwrap();
        let compute = pool_with(2);
        let (sender, receiver) = mpsc::channel();
        let end = io
            .spawn(|| vec![1_u8, 2, 3])
            .then_on(&compute.spawner(), move |bytes| {
                sender.send(ThreadRole::current()).unwrap();
                bytes.iter().map(|&b| u32::from(b)).sum::<u32>()
            });
        assert_eq!(end.wait(), 6);
        // The continuation ran on a compute worker, or on this thread while it helped the compute pool.
        let role = receiver.recv_timeout(PATIENCE).unwrap();
        assert!(
            role == ThreadRole::Worker || role == ThreadRole::Unknown,
            "{role}"
        );
        assert_ne!(role, ThreadRole::Io, "never on the IO pool");
    }

    #[test]
    fn a_job_can_go_back_and_forth_between_two_pools() {
        let io = ThreadPool::new(PoolConfig::io()).unwrap();
        let compute = pool_with(2);
        let (to_compute, to_io) = (compute.spawner(), io.spawner());
        let end = io
            .spawn(|| 1_u32)
            .then_on(&to_compute, |n| n + 10)
            .then_on(&to_io, |n| n + 100)
            .then_on(&to_compute, |n| n + 1_000);
        assert_eq!(end.wait(), 1_111);
    }

    #[test]
    fn a_panic_crosses_the_pools_without_running_the_continuation() {
        let io = ThreadPool::new(PoolConfig::io()).unwrap();
        let compute = pool_with(1);
        let ran = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&ran);
        let end =
            io.spawn(|| -> u32 { panic!("read failed") })
                .then_on(&compute.spawner(), move |n| {
                    flag.store(true, Ordering::SeqCst);
                    n
                });
        let failure = end.wait_catch().unwrap_err();
        assert_eq!(failure.message(), Some("read failed"));
        assert!(!ran.load(Ordering::SeqCst));
    }

    #[test]
    fn the_continuation_waits_in_the_queue_of_a_pool_without_workers() {
        let io = ThreadPool::new(PoolConfig::io()).unwrap();
        let single = pool_with(0);
        let end = io.spawn(|| 20).then_on(&single.spawner(), |n| n + 1);
        // The IO job runs on its own; the continuation then sits in the queue of the pool without workers
        // until somebody runs it. Waiting for the handle is exactly that.
        assert_eq!(end.wait(), 21);
        assert_eq!(single.pending_jobs(), 0);
    }

    #[test]
    fn a_waiter_on_a_pool_without_workers_finds_the_continuation_another_thread_queues() {
        // Regression test. The waiter blocks first (the queue of the pool without workers is empty and the
        // IO job it needs has not finished); then the IO worker finishes its job and queues the continuation
        // on that pool. Nobody else will ever run it, so the waiter must look at the queue again by itself.
        let io = ThreadPool::new(PoolConfig::io()).unwrap();
        let single = pool_with(0);
        let (release, gate) = mpsc::channel::<()>();
        let (started, running) = mpsc::channel::<()>();
        let end = io
            .spawn(move || {
                started.send(()).unwrap();
                gate.recv_timeout(PATIENCE)
                    .expect("the test releases the job");
                20
            })
            .then_on(&single.spawner(), |n| n + 1);
        running.recv_timeout(PATIENCE).unwrap();

        let (answer, answered) = mpsc::channel();
        let waiter = thread::spawn(move || answer.send(end.wait()).unwrap());
        thread::sleep(Duration::from_millis(20)); // let the waiter block
        release.send(()).unwrap();

        assert_eq!(
            answered
                .recv_timeout(PATIENCE)
                .expect("the waiter finished"),
            21
        );
        waiter.join().unwrap();
    }

    #[test]
    fn the_priority_applies_on_the_target_pool() {
        let io = pool_with(0);
        let target = pool_with(0);
        let order = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (for_low, for_high) = (Arc::clone(&order), Arc::clone(&order));
        let spawner = target.spawner();
        let low = io
            .spawn(|| ())
            .then_on_with_priority(&spawner, JobPriority::Low, move |()| {
                for_low.lock().unwrap().push("low");
            });
        let high = io
            .spawn(|| ())
            .then_on_with_priority(&spawner, JobPriority::High, move |()| {
                for_high.lock().unwrap().push("high");
            });
        io.run_pending();
        target.run_pending();
        assert!(low.is_done() && high.is_done());
        assert_eq!(*order.lock().unwrap(), ["high", "low"]);
    }
}

mod stop_tokens {
    use super::*;

    #[test]
    fn a_stop_request_wakes_a_sleeper_at_once() {
        let token = StopToken::new();
        let sleeper = {
            let token = token.clone();
            thread::spawn(move || {
                token.wait();
                Instant::now()
            })
        };
        thread::sleep(Duration::from_millis(5));
        let asked = Instant::now();
        token.request_stop();
        let woke = sleeper.join().unwrap();
        assert!(woke.saturating_duration_since(asked) < Duration::from_secs(30));
    }

    #[test]
    fn one_token_stops_many_threads() {
        let token = StopToken::new();
        let stopped = Arc::new(AtomicUsize::new(0));
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let (token, stopped) = (token.clone(), Arc::clone(&stopped));
                thread::spawn(move || {
                    token.wait();
                    stopped.fetch_add(1, Ordering::SeqCst);
                })
            })
            .collect();
        token.request_stop();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(stopped.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn a_timed_wait_ends_early_only_for_a_stop() {
        let token = StopToken::new();
        let started = Instant::now();
        assert!(!token.wait_timeout(Duration::from_millis(20)));
        assert!(started.elapsed() >= Duration::from_millis(15));
        token.request_stop();
        assert!(
            token.wait_timeout(PATIENCE),
            "a stopped token returns at once"
        );
    }
}

mod dedicated_threads {
    use super::*;

    #[test]
    fn the_thread_has_its_name_and_its_role() {
        let config = ThreadConfig::new("cv-render").with_role(ThreadRole::Render);
        let thread = DedicatedThread::spawn(config, |_| {
            (
                thread::current().name().map(str::to_owned),
                ThreadRole::current(),
            )
        })
        .unwrap();
        assert_eq!(thread.name(), "cv-render");
        assert_eq!(thread.role(), ThreadRole::Render);
        let (name, role) = thread.join();
        assert_eq!(name.as_deref(), Some("cv-render"));
        assert_eq!(role, ThreadRole::Render);
        assert_eq!(ThreadRole::current(), ThreadRole::Unknown);
    }

    #[test]
    fn join_returns_the_value_of_the_closure() {
        let thread =
            DedicatedThread::spawn(ThreadConfig::new("cv-answer"), |_| "forty-two".len()).unwrap();
        assert_eq!(thread.join(), 9);
    }

    #[test]
    fn a_loop_runs_until_it_is_asked_to_stop() {
        let ticks = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&ticks);
        let thread = DedicatedThread::spawn(ThreadConfig::new("cv-ticker"), move |stop| {
            let mut rounds = 0_usize;
            while !stop.wait_timeout(Duration::from_millis(1)) {
                counter.fetch_add(1, Ordering::SeqCst);
                rounds += 1;
            }
            rounds
        })
        .unwrap();
        eventually("the loop ticked", || ticks.load(Ordering::SeqCst) > 0);
        assert!(!thread.is_stop_requested());
        assert!(!thread.is_finished());
        let rounds = thread.stop_and_join();
        assert!(rounds > 0);
    }

    #[test]
    fn a_sleeping_thread_is_woken_by_the_stop_request() {
        let thread = DedicatedThread::spawn(ThreadConfig::new("cv-sleeper"), |stop| {
            stop.wait();
            "woken"
        })
        .unwrap();
        assert_eq!(thread.stop_and_join(), "woken");
    }

    #[test]
    fn is_finished_turns_true_when_the_closure_returns() {
        let thread = DedicatedThread::spawn(ThreadConfig::new("cv-short"), |_| ()).unwrap();
        eventually("the thread finished", || thread.is_finished());
        thread.join();
    }

    #[test]
    fn dropping_the_handle_stops_and_joins_the_thread() {
        let finished = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&finished);
        let thread = DedicatedThread::spawn(ThreadConfig::new("cv-guarded"), move |stop| {
            stop.wait();
            flag.store(true, Ordering::SeqCst);
        })
        .unwrap();
        assert!(!finished.load(Ordering::SeqCst));
        drop(thread);
        assert!(
            finished.load(Ordering::SeqCst),
            "the drop waited for the thread to end"
        );
    }

    #[test]
    fn a_detached_thread_outlives_its_handle_and_can_still_be_stopped() {
        let finished = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&finished);
        let thread = DedicatedThread::spawn(ThreadConfig::new("cv-free"), move |stop| {
            stop.wait();
            flag.store(true, Ordering::SeqCst);
        })
        .unwrap();
        let token = thread.detach();
        thread::sleep(Duration::from_millis(5));
        assert!(
            !finished.load(Ordering::SeqCst),
            "dropping the detached handle stopped nothing"
        );
        token.request_stop();
        eventually("the detached thread ended", || {
            finished.load(Ordering::SeqCst)
        });
    }

    #[test]
    fn the_stop_token_of_a_handle_can_stop_it_from_elsewhere() {
        let thread = DedicatedThread::spawn(ThreadConfig::new("cv-remote"), |stop| {
            stop.wait();
            7
        })
        .unwrap();
        let remote = thread.stop_token();
        assert!(!remote.is_stopped());
        thread::spawn(move || remote.request_stop()).join().unwrap();
        assert!(thread.is_stop_requested());
        assert_eq!(thread.join(), 7);
    }

    #[test]
    fn a_panic_is_returned_by_join_catch() {
        let thread = DedicatedThread::spawn(ThreadConfig::new("cv-bad"), |_| -> u32 {
            panic!("render thread lost the device")
        })
        .unwrap();
        let failure = thread.join_catch().unwrap_err();
        assert_eq!(failure.message(), Some("render thread lost the device"));
    }

    #[test]
    fn join_re_raises_the_original_payload() {
        let thread = DedicatedThread::spawn(ThreadConfig::new("cv-payload"), |_| -> u32 {
            std::panic::panic_any(42_u32)
        })
        .unwrap();
        let payload =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| thread.join())).unwrap_err();
        assert_eq!(payload.downcast_ref::<u32>(), Some(&42));
    }

    #[test]
    fn dropping_the_handle_of_a_panicked_thread_does_not_panic() {
        let thread = DedicatedThread::spawn(ThreadConfig::new("cv-quiet"), |_| -> u32 {
            panic!("ignored")
        })
        .unwrap();
        eventually("the thread ended", || thread.is_finished());
        drop(thread);
    }

    #[test]
    fn stop_and_join_catch_reports_a_panic_after_the_stop() {
        let thread = DedicatedThread::spawn(ThreadConfig::new("cv-late"), |stop| {
            stop.wait();
            panic!("failed while shutting down");
        })
        .unwrap();
        let failure: Result<(), _> = thread.stop_and_join_catch();
        assert_eq!(
            failure.unwrap_err().message(),
            Some("failed while shutting down")
        );
    }

    #[test]
    fn an_invalid_config_is_refused_before_a_thread_starts() {
        let started = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&started);
        let err = DedicatedThread::spawn(ThreadConfig::new(""), move |_| {
            flag.store(true, Ordering::SeqCst);
        })
        .unwrap_err();
        assert!(matches!(err, ThreadingError::InvalidConfig { .. }), "{err}");
        thread::sleep(Duration::from_millis(2));
        assert!(!started.load(Ordering::SeqCst));
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "Miri cannot call the OS thread priority API (SetThreadPriority and friends)"
    )]
    fn the_thread_asks_for_its_priority_and_works_even_if_the_os_refuses() {
        for priority in [
            ThreadPriority::Low,
            ThreadPriority::High,
            ThreadPriority::TimeCritical,
        ] {
            let thread = DedicatedThread::spawn(
                ThreadConfig::new("cv-priority").with_priority(priority),
                |_| 1 + 1,
            )
            .unwrap();
            assert_eq!(thread.join(), 2, "{priority:?}");
        }
    }

    #[test]
    fn a_custom_stack_size_is_accepted() {
        let thread = DedicatedThread::spawn(
            ThreadConfig::new("cv-stack").with_stack_size(2 * 1024 * 1024),
            |_| {
                // Uses a good part of a default-sized stack's worth of frames without overflowing the larger one.
                fn depth(n: u32) -> u32 {
                    if n == 0 { 0 } else { 1 + depth(n - 1) }
                }
                depth(if cfg!(miri) { 200 } else { 5_000 })
            },
        )
        .unwrap();
        assert_eq!(thread.join(), if cfg!(miri) { 200 } else { 5_000 });
    }

    #[test]
    fn a_dedicated_thread_can_use_a_pool_it_is_given() {
        let pool = Arc::new(pool_with(2));
        let for_thread = Arc::clone(&pool);
        let thread = DedicatedThread::spawn(
            ThreadConfig::new("cv-driver").with_role(ThreadRole::Custom("driver")),
            move |_| for_thread.spawn(|| 21 * 2).wait(),
        )
        .unwrap();
        assert_eq!(thread.join(), 42);
    }

    #[test]
    fn two_dedicated_threads_talk_through_a_channel() {
        let (sender, receiver) = mpsc::channel::<u32>();
        let consumer = DedicatedThread::spawn(
            ThreadConfig::new("cv-consumer").with_role(ThreadRole::Audio),
            move |stop| {
                let mut sum = 0;
                while !stop.is_stopped() {
                    if let Ok(value) = receiver.recv_timeout(Duration::from_millis(2)) {
                        sum += value;
                    }
                }
                // Whatever arrived before the stop is still in the channel.
                sum + receiver.try_iter().sum::<u32>()
            },
        )
        .unwrap();
        let producer = DedicatedThread::spawn(ThreadConfig::new("cv-producer"), move |_| {
            for value in 1..=10 {
                sender.send(value).unwrap();
            }
        })
        .unwrap();
        producer.join();
        assert_eq!(consumer.stop_and_join(), 55);
    }
}
