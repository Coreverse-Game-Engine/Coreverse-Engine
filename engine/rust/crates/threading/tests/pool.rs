//! Behaviour of `ThreadPool` and `JobHandle` seen from outside the crate.

use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use threading::{PoolConfig, ThreadPool, ThreadPriority, ThreadingError, WorkerCount};

/// A generous limit for "this must not hang"; tests only wait this long when something is broken.
const PATIENCE: Duration = Duration::from_secs(20);

/// How many items or repetitions a test uses: `normal` on a real run, `under_miri` under Miri, which runs
/// hundreds of times slower than native code. The behaviour tested is the same; only the amount of work changes.
const fn scaled(normal: usize, under_miri: usize) -> usize {
    if cfg!(miri) { under_miri } else { normal }
}

fn pool_with(workers: usize) -> ThreadPool {
    ThreadPool::new(PoolConfig::new().with_workers(WorkerCount::Exact(workers)))
        .expect("the pool starts")
}

#[test]
fn a_job_returns_its_value() {
    let pool = pool_with(2);
    assert_eq!(pool.spawn(|| 6 * 7).wait(), 42);
}

#[test]
fn a_job_can_return_a_non_copy_value() {
    let pool = pool_with(1);
    let text = pool.spawn(|| String::from("coreverse")).wait();
    assert_eq!(text, "coreverse");
}

#[test]
fn many_jobs_all_run_exactly_once() {
    let pool = pool_with(4);
    let counter = Arc::new(AtomicUsize::new(0));
    let jobs = scaled(1_000, 40);
    let handles: Vec<_> = (0..jobs)
        .map(|_| {
            let counter = Arc::clone(&counter);
            pool.spawn(move || counter.fetch_add(1, Ordering::SeqCst))
        })
        .collect();
    let mut seen: Vec<usize> = handles.into_iter().map(|handle| handle.wait()).collect();
    seen.sort_unstable();
    assert_eq!(
        seen,
        (0..jobs).collect::<Vec<_>>(),
        "each increment happened once"
    );
    assert_eq!(counter.load(Ordering::SeqCst), jobs);
}

#[test]
fn one_worker_runs_jobs_in_submission_order() {
    let pool = pool_with(1);
    let (log, order) = mpsc::channel();
    let handles: Vec<_> = (0..50)
        .map(|n| {
            let log = log.clone();
            pool.spawn(move || log.send(n).unwrap())
        })
        .collect();
    // The test thread must not call `wait` before the jobs are done: a waiting thread helps by running queued
    // jobs itself, and two threads running jobs at the same time can log them out of order. Receiving from a
    // channel keeps every job on the one worker.
    let seen: Vec<i32> = (0..50)
        .map(|_| order.recv_timeout(PATIENCE).expect("the job ran"))
        .collect();
    assert_eq!(seen, (0..50).collect::<Vec<_>>());
    for handle in handles {
        handle.wait();
    }
}

#[test]
fn is_done_turns_true_and_stays_true() {
    let pool = pool_with(1);
    let (release, gate) = mpsc::channel::<()>();
    let handle = pool.spawn(move || {
        gate.recv_timeout(PATIENCE).expect("released");
    });
    assert!(!handle.is_done(), "the job is blocked on the gate");
    release.send(()).unwrap();
    while !handle.is_done() {
        thread::yield_now();
    }
    assert!(handle.is_done());
    handle.wait();
}

#[test]
fn a_panicking_job_is_reported_through_wait_catch() {
    let pool = pool_with(2);
    let panic = pool
        .spawn(|| -> u32 { panic!("job failed on purpose") })
        .wait_catch()
        .unwrap_err();
    assert_eq!(panic.message(), Some("job failed on purpose"));
}

#[test]
fn wait_re_raises_the_panic_with_its_payload() {
    let pool = pool_with(1);
    // `black_box` keeps the argument from being folded into the format string at compile time, which would
    // turn the payload into a `&'static str` instead of a `String`.
    let value = std::hint::black_box(7);
    let handle = pool.spawn(move || -> u32 { panic!("formatted {value}") });
    let caught = panic::catch_unwind(AssertUnwindSafe(|| handle.wait())).unwrap_err();
    assert_eq!(
        caught.downcast_ref::<String>().map(String::as_str),
        Some("formatted 7")
    );
}

#[test]
fn the_pool_keeps_working_after_a_job_panicked() {
    let pool = pool_with(1);
    for _ in 0..5 {
        assert!(
            pool.spawn(|| -> u32 { panic!("again") })
                .wait_catch()
                .is_err()
        );
    }
    assert_eq!(pool.spawn(|| 1 + 1).wait(), 2, "the single worker survived");
}

/// Asks `count` jobs for the name of the thread they run on and collects the answers.
///
/// The test thread must not call `wait` on the handles: a waiting thread helps by running queued jobs itself
/// (see `JobHandle`), and then the name would be the test thread's. It receives from a channel instead, which
/// keeps every job on a worker.
fn worker_names(pool: &ThreadPool, count: usize) -> Vec<String> {
    let (send, receive) = mpsc::channel();
    // All jobs meet at a barrier, so each of them needs a worker of its own.
    let barrier = Arc::new(std::sync::Barrier::new(count));
    let handles: Vec<_> = (0..count)
        .map(|_| {
            let send = send.clone();
            let barrier = Arc::clone(&barrier);
            pool.spawn(move || {
                barrier.wait();
                let name = thread::current().name().map(str::to_owned);
                send.send(name).unwrap();
            })
        })
        .collect();
    drop(send);
    let mut names: Vec<String> = (0..count)
        .map(|_| {
            receive
                .recv_timeout(PATIENCE)
                .expect("a worker answered")
                .expect("workers are named")
        })
        .collect();
    for handle in handles {
        handle.wait();
    }
    names.sort();
    names
}

#[test]
fn workers_carry_the_configured_name() {
    let pool = ThreadPool::new(
        PoolConfig::new()
            .with_workers(WorkerCount::Exact(1))
            .with_name_prefix("t1-test"),
    )
    .unwrap();
    assert_eq!(worker_names(&pool, 1), ["t1-test-0"]);
}

#[test]
fn every_worker_gets_its_own_index() {
    let pool = ThreadPool::new(
        PoolConfig::new()
            .with_workers(WorkerCount::Exact(3))
            .with_name_prefix("idx"),
    )
    .unwrap();
    assert_eq!(pool.worker_count(), 3);
    assert_eq!(worker_names(&pool, 3), ["idx-0", "idx-1", "idx-2"]);
}

#[test]
#[cfg_attr(
    miri,
    ignore = "Miri cannot call the OS thread priority API (SetThreadPriority and friends)"
)]
fn a_lower_thread_priority_is_accepted() {
    // Lowering a priority is allowed everywhere; the point is that the pool starts and works.
    let pool = ThreadPool::new(
        PoolConfig::new()
            .with_workers(WorkerCount::Exact(1))
            .with_thread_priority(ThreadPriority::Low),
    )
    .unwrap();
    assert_eq!(pool.spawn(|| 5).wait(), 5);
}

#[test]
#[cfg_attr(
    miri,
    ignore = "Miri cannot call the OS thread priority API (SetThreadPriority and friends)"
)]
fn a_refused_priority_boost_does_not_stop_the_pool() {
    // Raising a priority needs a privilege on Linux; without it the worker keeps the default priority.
    let pool = ThreadPool::new(
        PoolConfig::new()
            .with_workers(WorkerCount::Exact(1))
            .with_thread_priority(ThreadPriority::High),
    )
    .unwrap();
    assert_eq!(pool.spawn(|| 5).wait(), 5);
}

#[test]
fn an_invalid_config_is_rejected_before_anything_starts() {
    let nul = ThreadPool::new(PoolConfig::new().with_name_prefix("a\0b"));
    assert!(matches!(nul, Err(ThreadingError::InvalidConfig { .. })));
    let many = ThreadPool::new(PoolConfig::new().with_workers(WorkerCount::Exact(1_000_000)));
    assert!(matches!(many, Err(ThreadingError::InvalidConfig { .. })));
}

#[test]
fn dropping_the_pool_finishes_the_queued_jobs() {
    let counter = Arc::new(AtomicUsize::new(0));
    let pool = pool_with(2);
    let jobs = scaled(200, 20);
    for _ in 0..jobs {
        let counter = Arc::clone(&counter);
        drop(pool.spawn(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        }));
    }
    drop(pool);
    assert_eq!(counter.load(Ordering::SeqCst), jobs);
}

#[test]
fn shutdown_finishes_the_queued_jobs_and_handles_stay_usable() {
    let pool = pool_with(1);
    let handles: Vec<_> = (0..20_u32).map(|n| pool.spawn(move || n + 1)).collect();
    pool.shutdown();
    let total: u32 = handles.into_iter().map(|handle| handle.wait()).sum();
    assert_eq!(total, (1..=20).sum());
}

#[test]
fn a_dropped_handle_does_not_cancel_the_job() {
    let counter = Arc::new(AtomicUsize::new(0));
    let pool = pool_with(1);
    let c = Arc::clone(&counter);
    drop(pool.spawn(move || {
        c.fetch_add(1, Ordering::SeqCst);
    }));
    pool.shutdown();
    assert_eq!(counter.load(Ordering::SeqCst), 1);
}

#[test]
fn a_job_can_wait_for_another_job_on_a_one_worker_pool() {
    let pool = Arc::new(pool_with(1));
    let inner_pool = Arc::clone(&pool);
    let outer = pool.spawn(move || {
        // The only worker is busy running this job; waiting for the inner job must run it right here.
        inner_pool.spawn(|| 20).wait() + 1
    });
    assert_eq!(outer.wait_catch().expect("no deadlock, no panic"), 21);
}

#[test]
fn jobs_can_be_spawned_from_many_threads_at_once() {
    let pool = pool_with(4);
    let counter = Arc::new(AtomicUsize::new(0));
    let per_thread = scaled(500, 25);
    thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| {
                let handles: Vec<_> = (0..per_thread)
                    .map(|_| {
                        let counter = Arc::clone(&counter);
                        pool.spawn(move || counter.fetch_add(1, Ordering::SeqCst))
                    })
                    .collect();
                for handle in handles {
                    handle.wait();
                }
            });
        }
    });
    assert_eq!(counter.load(Ordering::SeqCst), 4 * per_thread);
}

#[test]
fn the_last_owner_of_a_pool_can_be_one_of_its_own_jobs() {
    let pool = Arc::new(pool_with(2));
    let (done, finished) = mpsc::channel();
    let for_job = Arc::clone(&pool);
    drop(pool.spawn(move || {
        // By the time this runs, `main` has let go of its `Arc`; dropping this one stops the pool from
        // inside a worker, which must not try to join itself.
        thread::sleep(Duration::from_millis(50));
        drop(for_job);
        done.send(()).unwrap();
    }));
    drop(pool);
    finished
        .recv_timeout(PATIENCE)
        .expect("the job finished instead of deadlocking");
    // The worker that ran the job is detached (nobody can join it) and is still on its way out. Give it time to
    // leave: Miri reports threads that are still alive when the program ends.
    thread::sleep(Duration::from_millis(100));
}

mod single_threaded {
    use super::*;

    #[test]
    fn a_pool_without_workers_has_no_threads_and_runs_jobs_when_waited_for() {
        let pool = pool_with(0);
        assert_eq!(pool.worker_count(), 0);
        let handle = pool.spawn(|| thread::current().id());
        assert_eq!(pool.pending_jobs(), 1, "nothing runs it yet");
        assert!(!handle.is_done());
        assert_eq!(
            handle.wait(),
            thread::current().id(),
            "it ran on the waiting thread"
        );
        assert_eq!(pool.pending_jobs(), 0);
    }

    #[test]
    fn run_pending_runs_everything_in_order() {
        let pool = pool_with(0);
        let order = Arc::new(Mutex::new(Vec::new()));
        for n in 0..5 {
            let order = Arc::clone(&order);
            drop(pool.spawn(move || order.lock().unwrap().push(n)));
        }
        assert_eq!(pool.run_pending(), 5);
        assert_eq!(pool.run_pending(), 0);
        assert_eq!(*order.lock().unwrap(), [0, 1, 2, 3, 4]);
    }

    #[test]
    fn waiting_for_a_later_job_runs_the_earlier_ones_first() {
        let pool = pool_with(0);
        let order = Arc::new(Mutex::new(Vec::new()));
        let handles: Vec<_> = (0..3)
            .map(|n| {
                let order = Arc::clone(&order);
                pool.spawn(move || order.lock().unwrap().push(n))
            })
            .collect();
        let last = handles.into_iter().last().unwrap();
        last.wait();
        assert_eq!(*order.lock().unwrap(), [0, 1, 2]);
    }

    #[test]
    fn dropping_the_pool_runs_jobs_nobody_waited_for() {
        let counter = Arc::new(AtomicUsize::new(0));
        let pool = pool_with(0);
        let handle = {
            let counter = Arc::clone(&counter);
            pool.spawn(move || counter.fetch_add(1, Ordering::SeqCst))
        };
        drop(pool);
        assert!(
            handle.is_done(),
            "the pool finished the job while shutting down"
        );
        assert_eq!(handle.wait(), 0);
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_panic_is_reported_in_this_mode_too() {
        let pool = pool_with(0);
        let panic = pool
            .spawn(|| -> u8 { panic!("inline boom") })
            .wait_catch()
            .unwrap_err();
        assert_eq!(panic.message(), Some("inline boom"));
    }

    #[test]
    fn a_job_can_wait_for_a_job_it_spawns() {
        let pool = Arc::new(pool_with(0));
        let inner = Arc::clone(&pool);
        let outer = pool.spawn(move || inner.spawn(|| 2).wait() * 5);
        assert_eq!(outer.wait(), 10);
    }
}

#[test]
fn debug_output_names_the_state() {
    let pool = pool_with(0);
    let handle = pool.spawn(|| ());
    let pool_text = format!("{pool:?}");
    assert!(pool_text.contains("worker_count: 0"), "{pool_text}");
    assert!(pool_text.contains("pending_jobs: 1"), "{pool_text}");
    assert!(format!("{handle:?}").contains("done: false"));
    handle.wait();
}
