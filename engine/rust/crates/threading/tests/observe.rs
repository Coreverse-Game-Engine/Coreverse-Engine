//! Observer hooks, per-worker statistics and per-worker scratch memory (milestone T6), seen from outside the
//! crate.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use threading::{
    JobFinish, JobObserver, JobPriority, JobStart, JobSteal, PoolConfig, ThreadPool,
    ThreadingError, WorkerCount, WorkerSleep, WorkerWake, with_scratch,
};

/// How long a test waits for something that should happen at once before it calls it a hang.
const PATIENCE: Duration = Duration::from_secs(60);

fn config(workers: usize) -> PoolConfig {
    PoolConfig::new().with_workers(WorkerCount::Exact(workers))
}

/// Polls `condition` until it is true, or fails the test after `PATIENCE`.
fn eventually(what: &str, condition: impl Fn() -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for: {what}");
        thread::sleep(Duration::from_millis(1));
    }
}

/// One recorded event.
#[derive(Clone, Debug)]
enum Event {
    Start(JobStart),
    Finish(JobFinish),
    Steal(JobSteal),
    Sleep(WorkerSleep),
    Wake(WorkerWake),
}

/// Keeps every event, in the order the observer received them.
#[derive(Default)]
struct Recorder {
    events: Mutex<Vec<Event>>,
}

impl Recorder {
    fn push(&self, event: Event) {
        self.events.lock().unwrap().push(event);
    }

    fn events(&self) -> Vec<Event> {
        self.events.lock().unwrap().clone()
    }

    fn starts(&self) -> Vec<JobStart> {
        self.events()
            .into_iter()
            .filter_map(|event| match event {
                Event::Start(start) => Some(start),
                _ => None,
            })
            .collect()
    }

    fn finishes(&self) -> Vec<JobFinish> {
        self.events()
            .into_iter()
            .filter_map(|event| match event {
                Event::Finish(finish) => Some(finish),
                _ => None,
            })
            .collect()
    }

    fn steals(&self) -> Vec<JobSteal> {
        self.events()
            .into_iter()
            .filter_map(|event| match event {
                Event::Steal(steal) => Some(steal),
                _ => None,
            })
            .collect()
    }

    fn sleeps(&self) -> Vec<WorkerSleep> {
        self.events()
            .into_iter()
            .filter_map(|event| match event {
                Event::Sleep(sleep) => Some(sleep),
                _ => None,
            })
            .collect()
    }

    fn wakes(&self) -> Vec<WorkerWake> {
        self.events()
            .into_iter()
            .filter_map(|event| match event {
                Event::Wake(wake) => Some(wake),
                _ => None,
            })
            .collect()
    }
}

impl JobObserver for Recorder {
    fn job_started(&self, event: &JobStart) {
        self.push(Event::Start(*event));
    }

    fn job_finished(&self, event: &JobFinish) {
        self.push(Event::Finish(*event));
    }

    fn job_stolen(&self, event: &JobSteal) {
        self.push(Event::Steal(*event));
    }

    fn worker_sleeping(&self, event: &WorkerSleep) {
        self.push(Event::Sleep(*event));
    }

    fn worker_woke(&self, event: &WorkerWake) {
        self.push(Event::Wake(*event));
    }
}

fn observed(workers: usize) -> (ThreadPool, Arc<Recorder>) {
    let recorder = Arc::new(Recorder::default());
    let pool = ThreadPool::new(config(workers).with_observer(recorder.clone())).unwrap();
    (pool, recorder)
}

/// Runs `job` as a job of `pool` without the calling thread helping to run it, and returns its answer.
fn answer_on_a_worker<T: Send + 'static>(
    pool: &ThreadPool,
    job: impl FnOnce() -> T + Send + 'static,
) -> T {
    let (sender, receiver) = mpsc::channel();
    drop(pool.spawn(move || sender.send(job()).expect("the test is listening")));
    receiver.recv_timeout(PATIENCE).expect("a worker answers")
}

mod hooks {
    use super::*;

    #[test]
    fn every_job_is_reported_as_started_and_finished_once() {
        let (pool, recorder) = observed(2);
        let handles: Vec<_> = (0..20_u32).map(|n| pool.spawn(move || n)).collect();
        for handle in handles {
            handle.wait();
        }
        drop(pool); // the workers have left: every event has been delivered
        let starts = recorder.starts();
        let finishes = recorder.finishes();
        assert_eq!(starts.len(), 20);
        assert_eq!(finishes.len(), 20);
        let mut started: Vec<u64> = starts.iter().map(|start| start.job).collect();
        let mut finished: Vec<u64> = finishes.iter().map(|finish| finish.job).collect();
        started.sort_unstable();
        finished.sort_unstable();
        assert_eq!(started, (0..20).collect::<Vec<u64>>(), "numbered from 0");
        assert_eq!(started, finished, "every start has its finish");
    }

    #[test]
    fn a_job_on_a_worker_names_the_worker_and_its_priority() {
        let (pool, recorder) = observed(1);
        let (sender, receiver) = mpsc::channel();
        drop(pool.spawn_with_priority(JobPriority::High, move || sender.send(()).unwrap()));
        receiver.recv_timeout(PATIENCE).unwrap();
        drop(pool);
        let start = recorder.starts().remove(0);
        assert_eq!(start.worker, Some(0));
        assert_eq!(start.priority, JobPriority::High);
        assert_eq!(start.pool, recorder.finishes()[0].pool);
        let finish = recorder.finishes().remove(0);
        assert_eq!(finish.job, start.job);
        assert_eq!(finish.started, start.at);
    }

    #[test]
    fn the_duration_covers_the_time_the_job_ran() {
        let (pool, recorder) = observed(1);
        answer_on_a_worker(&pool, || thread::sleep(Duration::from_millis(30)));
        drop(pool);
        let finish = recorder.finishes().remove(0);
        assert!(
            finish.duration >= Duration::from_millis(25),
            "{:?}",
            finish.duration
        );
    }

    #[test]
    fn a_job_run_by_a_helping_thread_has_no_worker() {
        let (pool, recorder) = observed(0);
        pool.spawn(|| ()).wait();
        let start = recorder.starts().remove(0);
        assert_eq!(start.worker, None);
    }

    #[test]
    fn nested_jobs_sit_inside_the_span_of_the_job_that_ran_them() {
        // No workers: the outer job waits for the inner one and runs it on the same thread.
        let (pool, recorder) = observed(0);
        let spawner = pool.spawner();
        pool.spawn(move || spawner.spawn(|| ()).unwrap().wait())
            .wait();
        let order: Vec<(char, u64)> = recorder
            .events()
            .into_iter()
            .filter_map(|event| match event {
                Event::Start(start) => Some(('S', start.job)),
                Event::Finish(finish) => Some(('F', finish.job)),
                _ => None,
            })
            .collect();
        assert_eq!(order, [('S', 0), ('S', 1), ('F', 1), ('F', 0)]);
        let finishes = recorder.finishes();
        let (inner, outer) = (&finishes[0], &finishes[1]);
        assert!(outer.duration >= inner.duration);
    }

    #[test]
    fn an_idle_worker_that_takes_a_job_from_a_busy_one_reports_the_steal() {
        let (pool, recorder) = observed(2);
        let spawner = pool.spawner();
        let (done, finished) = mpsc::channel::<()>();
        let (release, gate) = mpsc::channel::<()>();
        let (started, running) = mpsc::channel::<Option<usize>>();

        // This job runs on one worker, queues three jobs into that worker's own queue, and then stays busy,
        // so the only way the three get to run is that the other worker steals them.
        drop(pool.spawn(move || {
            started.send(threading::worker_index()).unwrap();
            for _ in 0..3 {
                let done = done.clone();
                drop(spawner.spawn(move || done.send(()).unwrap()).unwrap());
            }
            gate.recv_timeout(PATIENCE)
                .expect("the test releases the busy job");
        }));
        let victim = running
            .recv_timeout(PATIENCE)
            .unwrap()
            .expect("the job runs on a worker");
        for _ in 0..3 {
            finished
                .recv_timeout(PATIENCE)
                .expect("the other worker ran the job");
        }
        release.send(()).unwrap();
        drop(pool);

        let steals = recorder.steals();
        assert!(steals.len() >= 3, "{steals:?}");
        assert!(steals.iter().all(|steal| steal.victim == victim));
        assert!(steals.iter().all(|steal| steal.thief == Some(1 - victim)));
    }

    #[test]
    fn sleep_and_wake_come_in_pairs_and_the_wake_says_how_long() {
        let (pool, recorder) = observed(1);
        eventually("the worker went to sleep", || !recorder.sleeps().is_empty());
        thread::sleep(Duration::from_millis(20));
        answer_on_a_worker(&pool, || ());
        drop(pool);
        let events = recorder.events();
        let sleeps = events
            .iter()
            .filter(|e| matches!(e, Event::Sleep(_)))
            .count();
        let wakes = recorder.wakes();
        assert_eq!(sleeps, wakes.len(), "every sleep is followed by a wake");
        assert!(
            wakes
                .iter()
                .any(|wake| wake.slept >= Duration::from_millis(15)),
            "{wakes:?}"
        );
        assert!(wakes.iter().all(|wake| wake.worker == 0));
    }

    #[test]
    fn one_observer_can_serve_two_pools_and_tell_them_apart() {
        let recorder = Arc::new(Recorder::default());
        let a = ThreadPool::new(config(1).with_observer(recorder.clone())).unwrap();
        let b = ThreadPool::new(PoolConfig::io().with_observer(recorder.clone())).unwrap();
        assert_ne!(a.id(), b.id());
        a.spawn(|| ()).wait();
        b.spawn(|| ()).wait();
        let (id_a, id_b) = (a.id(), b.id());
        drop(a);
        drop(b);
        let pools: Vec<_> = recorder.starts().iter().map(|start| start.pool).collect();
        assert!(pools.contains(&id_a) && pools.contains(&id_b));
    }

    #[test]
    fn a_spawner_reports_the_id_of_its_pool() {
        let pool = ThreadPool::new(config(0)).unwrap();
        assert_eq!(pool.spawner().id(), pool.id());
    }

    #[test]
    fn a_hook_that_panics_does_not_hurt_the_pool() {
        struct Panicky(AtomicUsize);

        impl JobObserver for Panicky {
            fn job_started(&self, _event: &JobStart) {
                self.0.fetch_add(1, Ordering::SeqCst);
                panic!("the profiler is broken");
            }

            fn job_finished(&self, _event: &JobFinish) {
                panic!("still broken");
            }
        }

        for workers in [0, 2] {
            let panicky = Arc::new(Panicky(AtomicUsize::new(0)));
            let pool = ThreadPool::new(config(workers).with_observer(panicky.clone())).unwrap();
            assert_eq!(pool.spawn(|| 40 + 2).wait(), 42, "{workers} workers");
            assert_eq!(pool.spawn(|| 1).wait(), 1);
            drop(pool);
            assert!(panicky.0.load(Ordering::SeqCst) >= 2);
        }
    }

    #[test]
    fn the_default_hooks_do_nothing() {
        struct Silent;
        impl JobObserver for Silent {}
        let pool = ThreadPool::new(config(1).with_observer(Arc::new(Silent))).unwrap();
        assert_eq!(pool.spawn(|| 3).wait(), 3);
    }

    #[test]
    fn a_config_with_an_observer_is_cloneable_and_printable() {
        let config = config(1).with_observer(Arc::new(Recorder::default()));
        assert!(config.observer().is_some());
        assert!(format!("{:?}", config.clone()).contains("JobObserver"));
        assert!(PoolConfig::new().observer().is_none());
    }
}

mod statistics {
    use super::*;

    #[test]
    fn every_job_is_counted_once_wherever_it_ran() {
        for workers in [0, 1, 3] {
            let pool = ThreadPool::new(config(workers)).unwrap();
            let handles: Vec<_> = (0..100_u32).map(|n| pool.spawn(move || n)).collect();
            for handle in handles {
                handle.wait();
            }
            let stats = pool.stats();
            assert_eq!(stats.total().jobs_run, 100, "{workers} workers");
            assert_eq!(stats.workers.len(), workers);
            let on_workers: u64 = stats.workers.iter().map(|worker| worker.jobs_run).sum();
            assert_eq!(on_workers + stats.helpers.jobs_run, 100);
        }
    }

    #[test]
    fn a_pool_without_workers_counts_everything_as_helper_work() {
        let pool = ThreadPool::new(config(0)).unwrap();
        for _ in 0..5 {
            pool.spawn(|| ()).wait();
        }
        let stats = pool.stats();
        assert!(stats.workers.is_empty());
        assert_eq!(stats.helpers.jobs_run, 5);
        assert_eq!(pool.spawner().stats().helpers.jobs_run, 5);
    }

    #[test]
    fn steals_and_stolen_from_add_up() {
        let pool = ThreadPool::new(config(2)).unwrap();
        let spawner = pool.spawner();
        let (done, finished) = mpsc::channel::<()>();
        let (release, gate) = mpsc::channel::<()>();
        let (started, running) = mpsc::channel::<()>();
        drop(pool.spawn(move || {
            started.send(()).unwrap();
            for _ in 0..4 {
                let done = done.clone();
                drop(spawner.spawn(move || done.send(()).unwrap()).unwrap());
            }
            gate.recv_timeout(PATIENCE).unwrap();
        }));
        running.recv_timeout(PATIENCE).unwrap();
        for _ in 0..4 {
            finished.recv_timeout(PATIENCE).unwrap();
        }
        release.send(()).unwrap();
        let stats = pool.stats();
        let stolen: u64 = stats.workers.iter().map(|worker| worker.steals).sum();
        let taken: u64 = stats.workers.iter().map(|worker| worker.stolen_from).sum();
        assert!(stolen >= 4, "{stats:?}");
        assert_eq!(stolen + stats.helpers.steals, taken, "{stats:?}");
    }

    #[test]
    fn time_is_measured_only_when_asked_for() {
        let off = ThreadPool::new(config(1)).unwrap();
        answer_on_a_worker(&off, || thread::sleep(Duration::from_millis(10)));
        assert_eq!(off.stats().total().busy, Duration::ZERO);

        let on = ThreadPool::new(config(1).with_timing(true)).unwrap();
        answer_on_a_worker(&on, || thread::sleep(Duration::from_millis(20)));
        eventually("the busy time is recorded", || {
            on.stats().total().busy >= Duration::from_millis(15)
        });
    }

    #[test]
    fn nested_jobs_count_once_in_the_busy_time() {
        // No workers: the outer job runs the inner one inside itself; the helper's busy time is the outer
        // job's time, not the sum of both.
        let pool = ThreadPool::new(config(0).with_timing(true)).unwrap();
        let spawner = pool.spawner();
        let started = Instant::now();
        pool.spawn(move || {
            spawner
                .spawn(|| thread::sleep(Duration::from_millis(30)))
                .unwrap()
                .wait();
        })
        .wait();
        let wall = started.elapsed();
        let busy = pool.stats().helpers.busy;
        assert!(busy >= Duration::from_millis(25), "{busy:?}");
        assert!(
            busy <= wall,
            "busy {busy:?} must not exceed the wall time {wall:?}"
        );
    }

    #[test]
    fn sleeps_are_counted_when_they_end() {
        let pool = ThreadPool::new(config(1).with_timing(true)).unwrap();
        thread::sleep(Duration::from_millis(30)); // let the worker fall asleep
        assert_eq!(
            pool.stats().workers[0].sleeps,
            0,
            "a sleep that is still going on is not counted yet"
        );
        answer_on_a_worker(&pool, || ()); // wakes the worker: its sleep ends
        eventually("the sleep is counted", || {
            pool.stats().workers[0].sleeps >= 1
        });
        assert!(pool.stats().workers[0].slept >= Duration::from_millis(20));
    }

    #[test]
    fn a_snapshot_is_a_value_that_can_be_compared_over_time() {
        let pool = ThreadPool::new(config(0)).unwrap();
        let before = pool.stats();
        pool.spawn(|| ()).wait();
        let after = pool.stats();
        assert_ne!(before, after);
        assert_eq!(
            after.total().jobs_run - before.total().jobs_run,
            1,
            "one job in between"
        );
    }
}

mod scratch {
    use super::*;

    fn with_arenas(workers: usize, bytes: usize) -> ThreadPool {
        ThreadPool::new(config(workers).with_scratch(bytes)).unwrap()
    }

    #[test]
    fn a_job_on_a_worker_gets_the_arena_of_its_thread() {
        let pool = with_arenas(2, 4096);
        let total = answer_on_a_worker(&pool, || {
            with_scratch(|arena| {
                let numbers = arena.alloc_slice_fill(100, 3_u32).unwrap();
                numbers.iter().sum::<u32>()
            })
        });
        assert_eq!(total, Some(300));
        assert_eq!(pool.stats().scratch_capacity, 4096);
    }

    #[test]
    fn other_threads_and_pools_without_scratch_get_none() {
        assert!(
            with_scratch(|_| ()).is_none(),
            "the test thread has no arena"
        );
        let plain = ThreadPool::new(config(1)).unwrap();
        assert_eq!(answer_on_a_worker(&plain, || with_scratch(|_| ())), None);
        assert_eq!(plain.stats().scratch_capacity, 0);
        // A pool without workers has no worker threads, so no arenas either.
        let single = with_arenas(0, 4096);
        assert_eq!(single.spawn(|| with_scratch(|_| ())).wait(), None);
    }

    #[test]
    fn a_new_frame_empties_every_arena_between_jobs() {
        let pool = with_arenas(1, 4096);
        let used = answer_on_a_worker(&pool, || {
            with_scratch(|arena| {
                arena.alloc_slice_fill(1_000, 0_u8).unwrap();
                arena.used()
            })
        });
        assert_eq!(used, Some(1_000));

        pool.begin_scratch_frame();
        eventually("the worker reset its arena", || {
            pool.stats().workers[0].scratch_frames >= 1
        });
        let stats = pool.stats().workers[0];
        assert_eq!(
            stats.scratch_peak, 1_000,
            "the frame that ended used 1000 bytes"
        );
        let used = answer_on_a_worker(&pool, || with_scratch(|arena| arena.used()));
        assert_eq!(used, Some(0), "the next frame starts empty");
    }

    #[test]
    fn a_frame_announced_right_after_a_job_is_never_missed() {
        // The worker is on its way to sleep while the frame is announced: it must still reset, not sleep through
        // the frame until some other job wakes it.
        let rounds: u64 = if cfg!(miri) { 8 } else { 300 };
        let pool = with_arenas(1, 4096);
        for round in 1..=rounds {
            let used = answer_on_a_worker(&pool, || {
                with_scratch(|arena| {
                    arena.alloc(1_u8).unwrap();
                    arena.used()
                })
            });
            assert_eq!(used, Some(1), "round {round} starts with an empty arena");
            pool.begin_scratch_frame();
            eventually("the worker reset its arena", || {
                pool.stats().workers[0].scratch_frames >= round
            });
        }
    }

    #[test]
    fn a_frame_announced_during_a_job_waits_until_the_job_is_done() {
        let pool = Arc::new(with_arenas(1, 4096));
        let for_job = Arc::clone(&pool);
        let intact = answer_on_a_worker(&pool, move || {
            with_scratch(|arena| {
                let slot = arena.alloc(7_u64).unwrap();
                for_job.begin_scratch_frame();
                // Give the worker every chance to reset: it must not, because this closure still has the arena.
                thread::sleep(Duration::from_millis(30));
                let again = arena.alloc(8_u64).unwrap();
                (*slot, *again, arena.used())
            })
        });
        assert_eq!(
            intact,
            Some((7, 8, 16)),
            "nothing was reset under the borrow"
        );
        eventually("the reset happens once the job is done", || {
            pool.stats().workers[0].scratch_frames >= 1
        });
        assert_eq!(pool.stats().workers[0].scratch_peak, 16);
    }

    #[test]
    fn many_jobs_share_the_arena_of_their_worker_one_after_another() {
        let pool = with_arenas(2, 64 * 1024);
        let failures = Arc::new(AtomicUsize::new(0));
        let handles: Vec<_> = (0..200_u32)
            .map(|n| {
                let failures = Arc::clone(&failures);
                pool.spawn(move || {
                    // On a worker the job has an arena; on a helping thread it falls back to the heap.
                    let sum = with_scratch(|arena| {
                        let values = arena.alloc_slice_fill(64, n).unwrap();
                        values.iter().sum::<u32>()
                    })
                    .unwrap_or(64 * n);
                    if sum != 64 * n {
                        failures.fetch_add(1, Ordering::SeqCst);
                    }
                    if n % 50 == 0 {
                        thread::yield_now();
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.wait();
        }
        assert_eq!(failures.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn jobs_inside_jobs_may_use_the_arena_of_the_same_worker() {
        // One worker: the outer job waits for a job it spawned; the worker runs that nested job inside the
        // outer one, and both use the same arena. Neither may see the other's memory change.
        let pool = with_arenas(1, 4096);
        let spawner = pool.spawner();
        let result = answer_on_a_worker(&pool, move || {
            with_scratch(|arena| {
                let outer = arena.alloc(1_u32).unwrap();
                let inner = spawner
                    .spawn(|| {
                        with_scratch(|arena| {
                            *arena.alloc(2_u32).unwrap() += 40;
                            arena.used()
                        })
                    })
                    .unwrap()
                    .wait();
                (*outer, inner)
            })
        });
        let (outer, inner) = result.expect("the job runs on the worker");
        assert_eq!(outer, 1);
        assert_eq!(
            inner,
            Some(8),
            "the nested job's allocation sits after the outer one"
        );
    }

    #[test]
    fn begin_scratch_frame_without_scratch_does_nothing() {
        let pool = ThreadPool::new(config(1)).unwrap();
        pool.begin_scratch_frame();
        pool.begin_scratch_frame();
        assert_eq!(pool.stats().total().scratch_frames, 0);
    }

    #[test]
    fn a_scratch_capacity_of_zero_is_refused() {
        let err = ThreadPool::new(config(1).with_scratch(0)).unwrap_err();
        assert!(matches!(err, ThreadingError::InvalidConfig { .. }), "{err}");
    }

    #[test]
    fn an_arena_that_cannot_be_allocated_is_an_error_of_new() {
        let err = ThreadPool::new(config(2).with_scratch(usize::MAX)).unwrap_err();
        assert!(matches!(err, ThreadingError::Scratch(_)), "{err}");
    }

    #[test]
    fn dropping_the_pool_frees_the_arenas_and_joins_the_workers() {
        let pool = with_arenas(2, 8192);
        answer_on_a_worker(&pool, || {
            with_scratch(|arena| arena.alloc(1_u8).map(|_| ()))
        });
        let done = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&done);
        let dropper = thread::spawn(move || {
            drop(pool);
            flag.store(true, Ordering::SeqCst);
        });
        eventually("the pool shut down", || done.load(Ordering::SeqCst));
        dropper.join().unwrap();
    }
}
