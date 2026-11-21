//! Priorities, per-worker queues, stealing and spawners (milestone T2), seen from outside the crate.
//!
//! The tests that look at *which* worker runs a job, or in which order, never call `wait` on the test thread
//! before the jobs are done: a waiting thread helps by running jobs itself (see `JobHandle`), which would make the
//! observed order depend on the test thread. They receive results over channels instead.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use threading::{
    JobPriority, PoolConfig, Spawner, ThreadPool, ThreadingError, WorkerCount, worker_index,
};

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

/// Runs `f` on a thread of its own and waits for it for at most `PATIENCE`.
///
/// A deadlock or a livelock then fails the test with a message that says what did not finish, instead of a test
/// run that never ends. When the time is up the thread is left behind (the test has failed anyway).
fn within_patience<T: Send + 'static>(what: &str, f: impl FnOnce() -> T + Send + 'static) -> T {
    let (send, receive) = mpsc::channel();
    let thread = thread::spawn(move || {
        let _ = send.send(f());
    });
    match receive.recv_timeout(PATIENCE) {
        Ok(value) => {
            thread.join().expect("the thread ended");
            value
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            panic!("{what} did not finish within {PATIENCE:?}: a deadlock or a livelock")
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => panic!("{what} panicked"),
    }
}

#[test]
fn spawners_can_be_cloned_shared_and_moved_into_jobs() {
    fn assert_traits<T: Send + Sync + Clone + 'static>() {}
    assert_traits::<Spawner>();
}

mod priorities {
    use super::*;

    #[test]
    fn single_threaded_mode_runs_higher_priorities_first() {
        let pool = pool_with(0);
        let order = Arc::new(std::sync::Mutex::new(Vec::new()));
        for (label, priority) in [
            ("low-1", JobPriority::Low),
            ("normal-1", JobPriority::Normal),
            ("high-1", JobPriority::High),
            ("low-2", JobPriority::Low),
            ("high-2", JobPriority::High),
            ("normal-2", JobPriority::Normal),
        ] {
            let order = Arc::clone(&order);
            drop(pool.spawn_with_priority(priority, move || order.lock().unwrap().push(label)));
        }
        assert_eq!(pool.run_pending(), 6);
        assert_eq!(
            *order.lock().unwrap(),
            ["high-1", "high-2", "normal-1", "normal-2", "low-1", "low-2"],
            "strict priority, first in first out within a priority"
        );
    }

    #[test]
    fn a_waiting_thread_helps_with_the_most_urgent_job_first() {
        let pool = pool_with(0);
        let order = Arc::new(std::sync::Mutex::new(Vec::new()));
        let record = |label: &'static str| {
            let order = Arc::clone(&order);
            move || order.lock().unwrap().push(label)
        };
        let low = pool.spawn_with_priority(JobPriority::Low, record("low"));
        drop(pool.spawn_with_priority(JobPriority::High, record("high")));
        low.wait();
        assert_eq!(*order.lock().unwrap(), ["high", "low"]);
    }

    #[test]
    fn a_busy_worker_picks_the_most_urgent_queued_job_next() {
        let pool = pool_with(1);
        let (started_send, started) = mpsc::channel();
        let (open, gate) = mpsc::channel::<()>();
        let blocker = pool.spawn(move || {
            started_send.send(()).unwrap();
            gate.recv_timeout(PATIENCE).expect("the gate opens");
        });
        started
            .recv_timeout(PATIENCE)
            .expect("the worker is busy now");

        let (log, order) = mpsc::channel();
        let mut handles = Vec::new();
        for (label, priority) in [
            ("low", JobPriority::Low),
            ("normal-1", JobPriority::Normal),
            ("high", JobPriority::High),
            ("normal-2", JobPriority::Normal),
        ] {
            let log = log.clone();
            handles.push(pool.spawn_with_priority(priority, move || log.send(label).unwrap()));
        }
        assert_eq!(pool.pending_jobs(), 4);
        open.send(()).unwrap();

        let seen: Vec<_> = (0..4)
            .map(|_| order.recv_timeout(PATIENCE).expect("the job ran"))
            .collect();
        assert_eq!(seen, ["high", "normal-1", "normal-2", "low"]);
        blocker.wait();
        for handle in handles {
            handle.wait();
        }
    }

    #[test]
    fn a_job_spawned_by_a_job_keeps_its_priority() {
        let pool = pool_with(1);
        let spawner = pool.spawner();
        let (log, order) = mpsc::channel();
        drop(pool.spawn(move || {
            for (label, priority) in [("low", JobPriority::Low), ("high", JobPriority::High)] {
                let log = log.clone();
                drop(
                    spawner
                        .spawn_with_priority(priority, move || log.send(label).unwrap())
                        .unwrap(),
                );
            }
        }));
        let seen: Vec<_> = (0..2)
            .map(|_| order.recv_timeout(PATIENCE).expect("the job ran"))
            .collect();
        assert_eq!(
            seen,
            ["high", "low"],
            "the newer but more urgent job goes first"
        );
    }
}

mod queues_and_stealing {
    use super::*;

    #[test]
    fn a_worker_runs_the_jobs_it_spawned_newest_first() {
        let pool = pool_with(1);
        let spawner = pool.spawner();
        let (log, order) = mpsc::channel();
        drop(pool.spawn(move || {
            for n in 0..5 {
                let log = log.clone();
                drop(spawner.spawn(move || log.send(n).unwrap()).unwrap());
            }
        }));
        let seen: Vec<_> = (0..5)
            .map(|_| order.recv_timeout(PATIENCE).expect("the job ran"))
            .collect();
        assert_eq!(seen, [4, 3, 2, 1, 0]);
    }

    #[test]
    fn an_idle_worker_steals_from_a_blocked_one() {
        let pool = pool_with(2);
        let spawner = pool.spawner();
        let (report, reported) = mpsc::channel();
        drop(pool.spawn(move || {
            let parent = worker_index();
            let (child_report, child_reported) = mpsc::channel();
            // The child goes into this worker's own queue ...
            drop(
                spawner
                    .spawn(move || child_report.send(worker_index()).unwrap())
                    .unwrap(),
            );
            // ... and this worker now blocks (it does not help), so the child can only run if the other
            // worker steals it.
            let child = child_reported
                .recv_timeout(PATIENCE)
                .expect("the other worker stole the child");
            report.send((parent, child)).unwrap();
        }));
        let (parent, child) = reported
            .recv_timeout(PATIENCE)
            .expect("the parent finished");
        assert!(parent.is_some() && child.is_some(), "both ran on workers");
        assert_ne!(parent, child, "the child ran on the other worker");
    }

    #[test]
    fn worker_index_is_none_outside_the_pool_and_in_range_inside() {
        assert_eq!(worker_index(), None);
        let pool = pool_with(3);
        let (report, reported) = mpsc::channel();
        drop(pool.spawn(move || report.send(worker_index()).unwrap()));
        let index = reported
            .recv_timeout(PATIENCE)
            .expect("the job ran")
            .expect("on a worker");
        assert!(index < 3, "{index}");
    }

    #[test]
    fn jobs_do_not_get_lost_while_workers_go_to_sleep() {
        let pool = pool_with(3);
        for round in 0..scaled(400, 30) {
            let (send, receive) = mpsc::channel();
            drop(pool.spawn(move || send.send(round).unwrap()));
            assert_eq!(receive.recv_timeout(PATIENCE).expect("woken"), round);
            if round % 20 == 0 {
                // Let the workers fall asleep, so that the wake-up path is the one that gets tested.
                thread::sleep(Duration::from_millis(2));
            }
        }
    }

    #[test]
    fn a_job_pushed_to_a_busy_workers_queue_wakes_a_sleeping_one() {
        let pool = pool_with(2);
        let spawner = pool.spawner();
        for _ in 0..scaled(200, 15) {
            let (done, finished) = mpsc::channel();
            let spawner = spawner.clone();
            drop(pool.spawn(move || {
                let (child_done, child_finished) = mpsc::channel();
                drop(spawner.spawn(move || child_done.send(()).unwrap()).unwrap());
                child_finished
                    .recv_timeout(PATIENCE)
                    .expect("a sleeping worker was woken");
                done.send(()).unwrap();
            }));
            finished.recv_timeout(PATIENCE).expect("the round finished");
            thread::sleep(Duration::from_micros(300));
        }
    }
}

/// Every node spawns `FAN` children, waits for them and counts itself.
const FAN: usize = 4;

fn fan_out(spawner: Spawner, counter: Arc<AtomicUsize>, depth: u32) {
    counter.fetch_add(1, Ordering::SeqCst);
    if depth == 0 {
        return;
    }
    let children: Vec<_> = (0..FAN)
        .map(|_| {
            let spawner_for_child = spawner.clone();
            let counter = Arc::clone(&counter);
            spawner
                .spawn(move || fan_out(spawner_for_child, counter, depth - 1))
                .expect("the pool is running")
        })
        .collect();
    for child in children {
        child.wait();
    }
}

mod spawners {
    use super::*;

    #[test]
    fn a_job_tree_that_waits_for_its_children_completes_at_every_pool_size() {
        let depth: u32 = if cfg!(miri) { 2 } else { 4 };
        let expected: usize = (0..=depth).map(|d| FAN.pow(d)).sum();
        for workers in [0, 1, 2, 4] {
            let pool = pool_with(workers);
            let counter = Arc::new(AtomicUsize::new(0));
            let root = {
                let spawner = pool.spawner();
                let counter = Arc::clone(&counter);
                pool.spawn(move || fan_out(spawner.clone(), counter, depth))
            };
            root.wait();
            assert_eq!(
                counter.load(Ordering::SeqCst),
                expected,
                "{workers} workers"
            );
        }
    }

    #[test]
    fn a_spawner_does_not_keep_the_pool_alive() {
        let pool = pool_with(1);
        let spawner = pool.spawner();
        assert_eq!(spawner.spawn(|| 1).unwrap().wait(), 1);
        drop(pool); // must not wait for the spawner
        let err = spawner.spawn(|| 2).unwrap_err();
        assert!(matches!(err, ThreadingError::PoolClosed), "{err}");
        let err = spawner
            .spawn_with_priority(JobPriority::High, || 2)
            .unwrap_err();
        assert!(matches!(err, ThreadingError::PoolClosed), "{err}");
    }

    #[test]
    fn jobs_spawned_by_running_jobs_are_accepted_while_the_pool_drains() {
        fn chain(spawner: Spawner, counter: Arc<AtomicUsize>, remaining: u32) {
            counter.fetch_add(1, Ordering::SeqCst);
            if remaining > 0 {
                let next = spawner.clone();
                let counter = Arc::clone(&counter);
                drop(
                    spawner
                        .spawn(move || chain(next, counter, remaining - 1))
                        .expect("accepted while draining"),
                );
            }
        }
        let links = scaled(50, 8) as u32;
        for workers in [0, 1, 3] {
            let pool = pool_with(workers);
            let counter = Arc::new(AtomicUsize::new(0));
            let spawner = pool.spawner();
            let c = Arc::clone(&counter);
            drop(pool.spawn(move || chain(spawner.clone(), c, links)));
            drop(pool); // returns only after the whole chain ran
            assert_eq!(
                counter.load(Ordering::SeqCst),
                links as usize + 1,
                "{workers} workers"
            );
        }
    }

    #[test]
    fn every_job_a_spawner_got_accepted_runs_even_if_the_pool_closes_meanwhile() {
        within_patience("the pool shutting down under four producers", || {
            let pool = pool_with(2);
            let ran = Arc::new(AtomicUsize::new(0));
            let accepted = Arc::new(AtomicUsize::new(0));
            let backlog = scaled(2_000, 20);
            let threads: Vec<_> = (0..4)
                .map(|_| {
                    let spawner = pool.spawner();
                    let ran = Arc::clone(&ran);
                    let accepted = Arc::clone(&accepted);
                    thread::spawn(move || {
                        loop {
                            // Backpressure: the producers must not run ahead of the workers by more than a
                            // bounded amount, or this test would queue millions of jobs.
                            while accepted.load(Ordering::SeqCst)
                                > ran.load(Ordering::SeqCst) + backlog
                            {
                                thread::yield_now();
                            }
                            let ran = Arc::clone(&ran);
                            match spawner.spawn(move || {
                                ran.fetch_add(1, Ordering::SeqCst);
                            }) {
                                Ok(handle) => {
                                    accepted.fetch_add(1, Ordering::SeqCst);
                                    drop(handle);
                                }
                                Err(ThreadingError::PoolClosed) => return,
                                Err(other) => panic!("unexpected error: {other}"),
                            }
                        }
                    })
                })
                .collect();
            // Close the pool once the producers have really been at work (a count, not a time, so that the
            // test does not depend on how fast or how slowly the threads are scheduled).
            while accepted.load(Ordering::SeqCst) < scaled(200, 8) {
                thread::yield_now();
            }
            drop(pool);
            for thread in threads {
                thread
                    .join()
                    .expect("the spawning thread ended after the pool closed");
            }
            assert!(accepted.load(Ordering::SeqCst) > 0);
            assert_eq!(
                ran.load(Ordering::SeqCst),
                accepted.load(Ordering::SeqCst),
                "no accepted job was lost"
            );
        });
    }

    /// Regression test: a pool must finish shutting down even while another thread keeps spawning.
    ///
    /// The shutdown used to wait until it saw the queue empty. A producer that spawns jobs a little faster than
    /// one worker runs them keeps the queue non-empty, so the workers never left, `drop(pool)` never returned,
    /// and the producer (which only stops once the pool is closed) never stopped: a livelock. Now a spawner
    /// refuses jobs from threads that are not running one of the pool's jobs as soon as the shutdown begins.
    ///
    /// The producer counts the jobs it gets accepted *after* the test has started to drop the pool. A few may
    /// slip in before the shutdown flag is set; with the bug it goes on until it reaches a limit that is far
    /// above that, and the test fails because the producer ended by its limit and not by a refusal.
    #[test]
    fn a_pool_shuts_down_even_while_another_thread_keeps_spawning() {
        let producer_got_a_refusal = within_patience("the pool shutting down", || {
            let pool = pool_with(1);
            let spawner = pool.spawner();
            let accepted = Arc::new(AtomicUsize::new(0));
            let dropping = Arc::new(AtomicBool::new(false));
            let lead = scaled(2_000, 30);
            let limit_after_drop = scaled(200_000, 10_000);
            // Each job costs about ten times what spawning one costs, so the one worker can never catch up
            // and the queue is never empty. Miri slows both down by about the same factor, so the job keeps
            // its size there.
            let spin = 3_000_u32;
            let producer = {
                let accepted = Arc::clone(&accepted);
                let dropping = Arc::clone(&dropping);
                thread::spawn(move || {
                    let mut after_drop = 0_usize;
                    loop {
                        let pool_is_being_dropped = dropping.load(Ordering::SeqCst);
                        if pool_is_being_dropped && after_drop >= limit_after_drop {
                            return false; // the shutdown waited for this thread: the bug
                        }
                        // The job: busy work that the optimizer must not remove.
                        let job = move || {
                            for i in 0..spin {
                                std::hint::black_box(i);
                            }
                        };
                        match spawner.spawn(job) {
                            Ok(handle) => {
                                drop(handle);
                                accepted.fetch_add(1, Ordering::Relaxed);
                                if pool_is_being_dropped {
                                    after_drop += 1;
                                }
                            }
                            Err(ThreadingError::PoolClosed) => return true,
                            Err(other) => panic!("unexpected error: {other}"),
                        }
                    }
                })
            };
            // Let a queue build up, then start to drop the pool while the producer is at full speed.
            while accepted.load(Ordering::Relaxed) < lead {
                thread::yield_now();
            }
            dropping.store(true, Ordering::SeqCst);
            drop(pool);
            producer.join().expect("the producer ended")
        });
        assert!(
            producer_got_a_refusal,
            "the producer went on and on after the shutdown began: it was not refused"
        );
    }

    #[test]
    fn a_spawner_refuses_other_threads_as_soon_as_the_shutdown_begins() {
        within_patience("the shutdown test", || {
            let pool = pool_with(1);
            let spawner = pool.spawner();
            let (started_send, started) = mpsc::channel();
            let (open, gate) = mpsc::channel::<()>();
            drop(pool.spawn(move || {
                started_send.send(()).unwrap();
                gate.recv_timeout(PATIENCE).expect("the gate opens");
            }));
            started.recv_timeout(PATIENCE).expect("the worker is busy");

            // The pool is dropped on another thread. It cannot finish while the gate is closed, because the
            // only worker is busy with the job that waits for it.
            let dropper = thread::spawn(move || drop(pool));

            // The shutdown has begun once the spawner starts to refuse. This thread runs no job of the pool.
            let deadline = Instant::now() + PATIENCE;
            loop {
                match spawner.spawn(|| ()) {
                    Err(ThreadingError::PoolClosed) => break,
                    Ok(handle) => drop(handle), // before the shutdown began; it will run
                    Err(other) => panic!("unexpected error: {other}"),
                }
                assert!(
                    Instant::now() < deadline,
                    "the spawner kept accepting jobs after the shutdown began"
                );
                thread::sleep(Duration::from_millis(1));
            }
            assert!(
                !dropper.is_finished(),
                "the refusal came from the shutdown that is still waiting for the busy worker"
            );
            open.send(()).unwrap();
            dropper.join().expect("the pool shut down");
        });
    }

    #[test]
    fn many_jobs_spawn_many_jobs_from_many_workers() {
        let pool = pool_with(4);
        let counter = Arc::new(AtomicUsize::new(0));
        let depth: u32 = if cfg!(miri) { 2 } else { 3 };
        let roots: Vec<_> = (0..scaled(16, 3))
            .map(|_| {
                let spawner = pool.spawner();
                let counter = Arc::clone(&counter);
                pool.spawn(move || fan_out(spawner.clone(), counter, depth))
            })
            .collect();
        for root in roots {
            root.wait();
        }
        let per_root: usize = (0..=depth).map(|d| FAN.pow(d)).sum();
        assert_eq!(counter.load(Ordering::SeqCst), scaled(16, 3) * per_root);
    }
}
