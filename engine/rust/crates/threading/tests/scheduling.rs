//! Priorities, per-worker queues, stealing and spawners (milestone T2), seen from outside the crate.
//!
//! The tests that look at *which* worker runs a job, or in which order, never call `wait` on the test thread
//! before the jobs are done: a waiting thread helps by running jobs itself (see `JobHandle`), which would make the
//! observed order depend on the test thread. They receive results over channels instead.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use threading::{
    JobPriority, PoolConfig, Spawner, ThreadPool, ThreadingError, WorkerCount, worker_index,
};

/// A generous limit for "this must not hang"; tests only wait this long when something is broken.
const PATIENCE: Duration = Duration::from_secs(20);

fn pool_with(workers: usize) -> ThreadPool {
    ThreadPool::new(PoolConfig::new().with_workers(WorkerCount::Exact(workers)))
        .expect("the pool starts")
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
        for round in 0..400_usize {
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
        for _ in 0..200 {
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
        const DEPTH: u32 = 4;
        let expected: usize = (0..=DEPTH).map(|d| FAN.pow(d)).sum();
        for workers in [0, 1, 2, 4] {
            let pool = pool_with(workers);
            let counter = Arc::new(AtomicUsize::new(0));
            let root = {
                let spawner = pool.spawner();
                let counter = Arc::clone(&counter);
                pool.spawn(move || fan_out(spawner.clone(), counter, DEPTH))
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
        for workers in [0, 1, 3] {
            let pool = pool_with(workers);
            let counter = Arc::new(AtomicUsize::new(0));
            let spawner = pool.spawner();
            let c = Arc::clone(&counter);
            drop(pool.spawn(move || chain(spawner.clone(), c, 50)));
            drop(pool); // returns only after the whole chain ran
            assert_eq!(counter.load(Ordering::SeqCst), 51, "{workers} workers");
        }
    }

    #[test]
    fn every_job_a_spawner_got_accepted_runs_even_if_the_pool_closes_meanwhile() {
        let pool = pool_with(2);
        let ran = Arc::new(AtomicUsize::new(0));
        let accepted = Arc::new(AtomicUsize::new(0));
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let spawner = pool.spawner();
                let ran = Arc::clone(&ran);
                let accepted = Arc::clone(&accepted);
                thread::spawn(move || {
                    loop {
                        // Backpressure: the producers must not outrun the workers by more than a bounded
                        // amount, or this test would queue millions of jobs in the 30 ms it runs.
                        while accepted.load(Ordering::SeqCst) > ran.load(Ordering::SeqCst) + 2_000 {
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
        thread::sleep(Duration::from_millis(30));
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
    }

    #[test]
    fn many_jobs_spawn_many_jobs_from_many_workers() {
        let pool = pool_with(4);
        let counter = Arc::new(AtomicUsize::new(0));
        let roots: Vec<_> = (0..16)
            .map(|_| {
                let spawner = pool.spawner();
                let counter = Arc::clone(&counter);
                pool.spawn(move || fan_out(spawner.clone(), counter, 3))
            })
            .collect();
        for root in roots {
            root.wait();
        }
        let per_root: usize = (0..=3).map(|d| FAN.pow(d)).sum();
        assert_eq!(counter.load(Ordering::SeqCst), 16 * per_root);
    }
}
