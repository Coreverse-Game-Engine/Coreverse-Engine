//! Dependencies between jobs and task graphs (milestone T4), seen from outside the crate.

use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use threading::{
    JobPriority, PoolConfig, TaskGraph, TaskId, ThreadPool, ThreadingError, WorkerCount,
};

/// Pool sizes every behaviour test runs on: no workers, one, a few, more than the machine probably has.
const SIZES: [usize; 4] = [0, 1, 3, 8];

/// How many items or repetitions a test uses: `normal` on a real run, `under_miri` under Miri, which runs
/// hundreds of times slower than native code. The behaviour tested is the same; only the amount of work changes.
const fn scaled(normal: usize, under_miri: usize) -> usize {
    if cfg!(miri) { under_miri } else { normal }
}

fn pool_with(workers: usize) -> ThreadPool {
    ThreadPool::new(PoolConfig::new().with_workers(WorkerCount::Exact(workers)))
        .expect("the pool starts")
}

/// The `&str` message of a caught panic.
fn message(payload: &(dyn std::any::Any + Send)) -> Option<&str> {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
}

/// Counts its own drops.
struct DropCounter<'a>(&'a AtomicUsize);

impl Drop for DropCounter<'_> {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

/// A small deterministic generator, so the random graphs are the same on every run.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }

    fn below(&mut self, bound: usize) -> usize {
        usize::try_from(self.next()).expect("fits") % bound
    }
}

mod continuations {
    use super::*;

    #[test]
    fn then_passes_the_result_on() {
        for workers in SIZES {
            let pool = pool_with(workers);
            let text = pool
                .spawn(|| 20)
                .then(|n| n + 1)
                .then(|n| format!("answer {}", n * 2));
            assert_eq!(text.wait(), "answer 42", "{workers} workers");
        }
    }

    #[test]
    fn then_works_on_a_job_that_is_done_already() {
        for workers in SIZES {
            let pool = pool_with(workers);
            let first = pool.spawn(|| 5);
            pool.run_pending();
            while !first.is_done() {
                thread::yield_now();
            }
            let second = first.then(|n| n * 2);
            assert_eq!(second.wait(), 10, "{workers} workers");
        }
    }

    #[test]
    fn a_long_chain_runs_in_order_and_finishes() {
        let length = scaled(2_000, 40);
        for workers in SIZES {
            let pool = pool_with(workers);
            let mut handle = pool.spawn(|| 0_usize);
            for _ in 0..length {
                handle = handle.then(|n| n + 1);
            }
            assert_eq!(handle.wait(), length, "{workers} workers");
        }
    }

    #[test]
    fn a_continuation_does_not_block_a_worker_while_it_waits() {
        // One worker, one job that is held back: if the continuation occupied the worker while waiting for
        // it, the unrelated job below could never run.
        let pool = pool_with(1);
        let (release, gate) = mpsc::channel::<()>();
        let (started, running) = mpsc::channel::<()>();
        let blocked = pool.spawn(move || {
            started.send(()).expect("the test is listening");
            gate.recv().expect("the test releases the job");
            1
        });
        running.recv().expect("the first job started");
        let chained = blocked.then(|n| n + 1);
        // The worker is busy with `blocked`; this thread runs the unrelated job itself while it waits.
        assert_eq!(pool.spawn(|| "free").wait(), "free");
        release.send(()).expect("the job is waiting");
        assert_eq!(chained.wait(), 2);
    }

    #[test]
    fn a_panic_skips_the_continuation_and_reaches_the_end_of_the_chain() {
        for workers in SIZES {
            let pool = pool_with(workers);
            let ran = Arc::new(AtomicBool::new(false));
            let flag = Arc::clone(&ran);
            let end = pool
                .spawn(|| -> u32 { panic!("first link") })
                .then(move |n| {
                    flag.store(true, Ordering::SeqCst);
                    n
                })
                .then(|n| n + 1);
            let failure = end.wait_catch().expect_err("the panic comes through");
            assert_eq!(failure.message(), Some("first link"), "{workers} workers");
            assert!(!ran.load(Ordering::SeqCst), "{workers} workers");
        }
    }

    #[test]
    fn a_panic_in_a_continuation_is_reported_and_the_pool_survives() {
        for workers in SIZES {
            let pool = pool_with(workers);
            let failure = pool
                .spawn(|| 1)
                .then(|_| -> u32 { panic!("second link") })
                .wait_catch()
                .expect_err("the continuation panicked");
            assert_eq!(failure.message(), Some("second link"));
            assert_eq!(pool.spawn(|| 7).wait(), 7, "{workers} workers");
        }
    }

    #[test]
    fn wait_re_raises_the_original_payload_of_a_chain() {
        let pool = pool_with(2);
        let end = pool
            .spawn(|| -> u32 { std::panic::panic_any(42_u32) })
            .then(|n| n);
        let payload = panic::catch_unwind(AssertUnwindSafe(|| end.wait())).unwrap_err();
        assert_eq!(payload.downcast_ref::<u32>(), Some(&42));
    }

    #[test]
    fn continuations_of_different_priorities_run_most_urgent_first() {
        // No workers: everything runs in `run_pending`, in the order the queue gives it out.
        let pool = pool_with(0);
        let order = Arc::new(Mutex::new(Vec::new()));
        let for_low = Arc::clone(&order);
        let for_high = Arc::clone(&order);
        let low = pool
            .spawn(|| ())
            .then_with_priority(JobPriority::Low, move |()| {
                for_low.lock().unwrap().push("low")
            });
        let high = pool
            .spawn(|| ())
            .then_with_priority(JobPriority::High, move |()| {
                for_high.lock().unwrap().push("high");
            });
        pool.run_pending();
        assert!(low.is_done() && high.is_done());
        assert_eq!(*order.lock().unwrap(), ["high", "low"]);
    }

    #[test]
    fn a_continuation_can_start_more_jobs() {
        for workers in SIZES {
            let pool = pool_with(workers);
            let spawner = pool.spawner();
            let end = pool.spawn(|| 3).then(move |n| {
                let inner = spawner.spawn(move || n * 10).expect("the pool runs");
                inner.wait() + 1
            });
            assert_eq!(end.wait(), 31, "{workers} workers");
        }
    }

    #[test]
    fn dropping_a_continuation_handle_does_not_cancel_it() {
        let ran = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&ran);
        let pool = pool_with(2);
        drop(pool.spawn(|| ()).then(move |()| {
            counter.fetch_add(1, Ordering::SeqCst);
        }));
        pool.shutdown();
        assert_eq!(ran.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn shutdown_finishes_a_continuation_queued_by_a_running_job() {
        let ran = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&ran);
        let pool = pool_with(0);
        let handle = pool.spawn(|| ()).then(move |()| {
            counter.fetch_add(1, Ordering::SeqCst);
        });
        // Nobody ran anything yet; dropping the pool runs the job and its continuation.
        drop(pool);
        assert!(handle.is_done());
        assert_eq!(ran.load(Ordering::SeqCst), 1);
    }
}

mod when_all {
    use super::*;

    #[test]
    fn the_results_come_back_in_input_order() {
        let count = scaled(64, 8);
        for workers in SIZES {
            let pool = pool_with(workers);
            let handles: Vec<_> = (0..count)
                .map(|n| {
                    pool.spawn(move || {
                        // Finish in a scrambled order.
                        if n % 3 == 0 {
                            thread::sleep(Duration::from_micros(200));
                        }
                        n * n
                    })
                })
                .collect();
            let all = pool.when_all(handles).wait();
            let expected: Vec<usize> = (0..count).map(|n| n * n).collect();
            assert_eq!(all, expected, "{workers} workers");
        }
    }

    #[test]
    fn an_empty_group_is_done_at_once() {
        let pool = pool_with(0);
        let none = pool.when_all(Vec::<threading::JobHandle<u8>>::new());
        assert!(none.is_done());
        assert!(none.wait().is_empty());
    }

    #[test]
    fn jobs_that_are_done_already_are_fine() {
        let pool = pool_with(0);
        let handles: Vec<_> = (0..5_u32).map(|n| pool.spawn(move || n)).collect();
        pool.run_pending();
        assert_eq!(pool.when_all(handles).wait(), [0, 1, 2, 3, 4]);
    }

    #[test]
    fn the_first_panic_in_input_order_wins_after_everything_finished() {
        for workers in SIZES {
            let pool = pool_with(workers);
            let finished = Arc::new(AtomicUsize::new(0));
            let handles: Vec<_> = (0..6_u32)
                .map(|n| {
                    let finished = Arc::clone(&finished);
                    pool.spawn(move || {
                        finished.fetch_add(1, Ordering::SeqCst);
                        if n == 4 {
                            panic!("four");
                        }
                        if n == 2 {
                            panic!("two");
                        }
                        n
                    })
                })
                .collect();
            let failure = pool.when_all(handles).wait_catch().unwrap_err();
            assert_eq!(failure.message(), Some("two"), "{workers} workers");
            assert_eq!(finished.load(Ordering::SeqCst), 6, "{workers} workers");
        }
    }

    #[test]
    fn a_continuation_runs_after_the_whole_group() {
        for workers in SIZES {
            let pool = pool_with(workers);
            let parts: Vec<_> = (1..=4_u64).map(|n| pool.spawn(move || n * 10)).collect();
            let total = pool
                .when_all(parts)
                .then(|values| values.iter().sum::<u64>());
            assert_eq!(total.wait(), 100, "{workers} workers");
        }
    }

    #[test]
    fn a_spawner_can_join_a_group_too() {
        let pool = pool_with(2);
        let spawner = pool.spawner();
        let handles: Vec<_> = (0..3_u8)
            .map(|n| spawner.spawn(move || n).unwrap())
            .collect();
        assert_eq!(spawner.when_all(handles).wait(), [0, 1, 2]);
    }
}

mod graphs {
    use super::*;

    /// Records which tasks ran, in the order they started.
    struct Log(Mutex<Vec<usize>>);

    impl Log {
        fn new() -> Self {
            Self(Mutex::new(Vec::new()))
        }

        fn push(&self, task: usize) {
            self.0.lock().unwrap().push(task);
        }

        fn take(&self) -> Vec<usize> {
            std::mem::take(&mut *self.0.lock().unwrap())
        }
    }

    #[test]
    fn an_empty_graph_runs_and_does_nothing() {
        for workers in SIZES {
            let pool = pool_with(workers);
            let mut graph = TaskGraph::new();
            pool.run_graph(&mut graph);
            assert!(pool.run_graph_catch(&mut graph).is_ok());
        }
    }

    #[test]
    fn a_diamond_respects_its_dependencies() {
        for workers in SIZES {
            let pool = pool_with(workers);
            let log = Log::new();
            let log = &log;
            let mut graph = TaskGraph::new();
            let a = graph.add("a", move || log.push(0));
            let b = graph.add_after("b", &[a], move || log.push(1)).unwrap();
            let c = graph.add_after("c", &[a], move || log.push(2)).unwrap();
            graph.add_after("d", &[b, c], move || log.push(3)).unwrap();
            pool.run_graph(&mut graph);
            let order = log.take();
            assert_eq!(order.first(), Some(&0), "{workers} workers: {order:?}");
            assert_eq!(order.last(), Some(&3), "{workers} workers: {order:?}");
            assert_eq!(order.len(), 4, "{workers} workers: {order:?}");
        }
    }

    #[test]
    fn a_random_dag_runs_every_task_once_after_all_its_dependencies() {
        let tasks = scaled(300, 25);
        for workers in SIZES {
            let pool = pool_with(workers);
            let mut random = Lcg(0x5eed);
            let done: Vec<AtomicBool> = (0..tasks).map(|_| AtomicBool::new(false)).collect();
            let runs: Vec<AtomicUsize> = (0..tasks).map(|_| AtomicUsize::new(0)).collect();
            let violations = AtomicUsize::new(0);
            let mut graph = TaskGraph::new();
            let mut ids: Vec<TaskId> = Vec::new();
            for index in 0..tasks {
                let mut dependencies = Vec::new();
                if index > 0 {
                    for _ in 0..random.below(4) {
                        dependencies.push(random.below(index));
                    }
                }
                dependencies.sort_unstable();
                dependencies.dedup();
                let (done, runs, violations) = (&done, &runs, &violations);
                let own_dependencies = dependencies.clone();
                let after: Vec<TaskId> = dependencies.iter().map(|&d| ids[d]).collect();
                let id = graph
                    .add_after(format!("task {index}"), &after, move || {
                        // Every dependency must have finished before this task starts.
                        if own_dependencies
                            .iter()
                            .any(|&d| !done[d].load(Ordering::SeqCst))
                        {
                            violations.fetch_add(1, Ordering::SeqCst);
                        }
                        runs[index].fetch_add(1, Ordering::SeqCst);
                        done[index].store(true, Ordering::SeqCst);
                    })
                    .unwrap();
                ids.push(id);
            }
            pool.run_graph(&mut graph);
            assert_eq!(violations.load(Ordering::SeqCst), 0, "{workers} workers");
            assert!(
                runs.iter().all(|runs| runs.load(Ordering::SeqCst) == 1),
                "{workers} workers: every task runs exactly once"
            );
        }
    }

    #[test]
    fn without_workers_the_tasks_run_in_topological_order() {
        let pool = pool_with(0);
        let log = Log::new();
        let log = &log;
        let mut graph = TaskGraph::new();
        let a = graph.add("a", move || log.push(0));
        let b = graph.add("b", move || log.push(1));
        let c = graph.add_after("c", &[b], move || log.push(2)).unwrap();
        let d = graph.add_after("d", &[a], move || log.push(3)).unwrap();
        graph.add_after("e", &[c, d], move || log.push(4)).unwrap();
        graph.add("f", move || log.push(5));
        pool.run_graph(&mut graph);
        let expected: Vec<usize> = graph
            .topological_order()
            .iter()
            .map(|id| id.index())
            .collect();
        assert_eq!(log.take(), expected);
    }

    #[test]
    fn priorities_decide_among_ready_tasks_without_workers() {
        let pool = pool_with(0);
        let log = Log::new();
        let log = &log;
        let mut graph = TaskGraph::new();
        let low = graph.add("low", move || log.push(0));
        let high = graph.add("high", move || log.push(1));
        let normal = graph.add("normal", move || log.push(2));
        graph.set_priority(low, JobPriority::Low).unwrap();
        graph.set_priority(high, JobPriority::High).unwrap();
        pool.run_graph(&mut graph);
        let _ = normal;
        assert_eq!(log.take(), [1, 2, 0]);
    }

    #[test]
    fn a_priority_never_lets_a_task_jump_its_dependencies() {
        let pool = pool_with(0);
        let log = Log::new();
        let log = &log;
        let mut graph = TaskGraph::new();
        let first = graph.add("first", move || log.push(0));
        let urgent = graph
            .add_after("urgent", &[first], move || log.push(1))
            .unwrap();
        graph.set_priority(urgent, JobPriority::High).unwrap();
        graph.set_priority(first, JobPriority::Low).unwrap();
        pool.run_graph(&mut graph);
        assert_eq!(log.take(), [0, 1]);
    }

    #[test]
    fn tasks_borrow_and_mutate_the_callers_data() {
        for workers in SIZES {
            let pool = pool_with(workers);
            let mut squares = vec![0_usize; 32];
            let mut graph = TaskGraph::new();
            for (n, slot) in squares.iter_mut().enumerate() {
                graph.add("square", move || *slot = n * n);
            }
            pool.run_graph(&mut graph);
            drop(graph);
            let expected: Vec<usize> = (0..32).map(|n| n * n).collect();
            assert_eq!(squares, expected, "{workers} workers");
        }
    }

    #[test]
    fn a_graph_can_be_run_again_and_again() {
        for workers in SIZES {
            let pool = pool_with(workers);
            let count = AtomicUsize::new(0);
            let count = &count;
            let mut graph = TaskGraph::new();
            let a = graph.add("a", move || {
                count.fetch_add(1, Ordering::SeqCst);
            });
            let b = graph
                .add_after("b", &[a], move || {
                    count.fetch_add(10, Ordering::SeqCst);
                })
                .unwrap();
            graph
                .add_after("c", &[a, b], move || {
                    count.fetch_add(100, Ordering::SeqCst);
                })
                .unwrap();
            for round in 1..=3 {
                pool.run_graph(&mut graph);
                assert_eq!(
                    count.load(Ordering::SeqCst),
                    111 * round,
                    "{workers} workers"
                );
            }
        }
    }

    #[test]
    fn a_panic_skips_the_dependents_and_lets_the_rest_run() {
        for workers in SIZES {
            let pool = pool_with(workers);
            let log = Log::new();
            let log = &log;
            let mut graph = TaskGraph::new();
            let bad = graph.add("bad", || panic!("no input"));
            let child = graph
                .add_after("child", &[bad], move || log.push(1))
                .unwrap();
            graph
                .add_after("grandchild", &[child], move || log.push(2))
                .unwrap();
            let fine = graph.add("fine", move || log.push(3));
            // Depends on both a failed and a healthy task: skipped all the same.
            graph
                .add_after("mixed", &[bad, fine], move || log.push(4))
                .unwrap();
            graph
                .add_after("after fine", &[fine], move || log.push(5))
                .unwrap();

            let failure = pool.run_graph_catch(&mut graph).unwrap_err();
            assert_eq!(failure.task_name(), "bad", "{workers} workers");
            assert_eq!(failure.task().index(), 0);
            assert_eq!(failure.panic().message(), Some("no input"));
            assert_eq!(failure.skipped_tasks(), 3, "{workers} workers");
            let mut ran = log.take();
            ran.sort_unstable();
            assert_eq!(ran, [3, 5], "{workers} workers");
        }
    }

    #[test]
    fn the_failed_task_with_the_smallest_index_is_reported() {
        for workers in SIZES {
            let pool = pool_with(workers);
            let mut graph = TaskGraph::new();
            graph.add("ok", || ());
            graph.add("first bad", || panic!("one"));
            graph.add("second bad", || panic!("two"));
            let failure = pool.run_graph_catch(&mut graph).unwrap_err();
            assert_eq!(failure.task_name(), "first bad", "{workers} workers");
            assert_eq!(failure.panic().message(), Some("one"));
            assert_eq!(failure.skipped_tasks(), 0);
        }
    }

    #[test]
    fn run_graph_re_raises_the_original_payload_and_the_pool_survives() {
        for workers in SIZES {
            let pool = pool_with(workers);
            let mut graph = TaskGraph::new();
            graph.add("bad", || panic!("boom {}", 7));
            let payload = panic::catch_unwind(AssertUnwindSafe(|| pool.run_graph(&mut graph)))
                .expect_err("the panic comes through");
            assert_eq!(message(&*payload), Some("boom 7"), "{workers} workers");
            assert_eq!(pool.spawn(|| 1).wait(), 1, "{workers} workers");
        }
    }

    #[test]
    fn a_graph_that_failed_runs_again_from_the_start() {
        let pool = pool_with(2);
        let attempts = AtomicUsize::new(0);
        let after = AtomicUsize::new(0);
        let (attempts, after) = (&attempts, &after);
        let mut graph = TaskGraph::new();
        let flaky = graph.add("flaky", move || {
            if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                panic!("first time");
            }
        });
        graph
            .add_after("after", &[flaky], move || {
                after.fetch_add(1, Ordering::SeqCst);
            })
            .unwrap();
        assert!(pool.run_graph_catch(&mut graph).is_err());
        assert_eq!(after.load(Ordering::SeqCst), 0);
        assert!(pool.run_graph_catch(&mut graph).is_ok());
        assert_eq!(after.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn skipped_tasks_are_dropped_with_the_graph() {
        let drops = AtomicUsize::new(0);
        let pool = pool_with(2);
        let mut graph = TaskGraph::new();
        let bad = graph.add("bad", || panic!("x"));
        for _ in 0..3 {
            let counter = DropCounter(&drops);
            graph
                .add_after("skipped", &[bad], move || {
                    let _keep = &counter;
                })
                .unwrap();
        }
        let _ = pool.run_graph_catch(&mut graph);
        assert_eq!(drops.load(Ordering::SeqCst), 0, "the graph still owns them");
        drop(graph);
        assert_eq!(drops.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn a_long_chain_and_a_wide_fan_out_both_finish() {
        let size = scaled(1_000, 30);
        for workers in SIZES {
            let pool = pool_with(workers);
            let count = AtomicUsize::new(0);
            let count = &count;
            let mut graph = TaskGraph::with_capacity(2 * size);
            let mut previous = graph.add("link", move || {
                count.fetch_add(1, Ordering::Relaxed);
            });
            for _ in 1..size {
                previous = graph
                    .add_after("link", &[previous], move || {
                        count.fetch_add(1, Ordering::Relaxed);
                    })
                    .unwrap();
            }
            for _ in 0..size {
                graph
                    .add_after("leaf", &[previous], move || {
                        count.fetch_add(1, Ordering::Relaxed);
                    })
                    .unwrap();
            }
            pool.run_graph(&mut graph);
            assert_eq!(count.load(Ordering::Relaxed), 2 * size, "{workers} workers");
        }
    }

    #[test]
    fn a_failure_at_the_head_of_a_long_chain_skips_it_all_without_recursion() {
        let size = scaled(20_000, 200);
        let pool = pool_with(1);
        let mut graph = TaskGraph::with_capacity(size);
        let mut previous = graph.add("head", || panic!("head"));
        for _ in 1..size {
            previous = graph.add_after("link", &[previous], || ()).unwrap();
        }
        let failure = pool.run_graph_catch(&mut graph).unwrap_err();
        assert_eq!(failure.skipped_tasks(), size - 1);
    }

    #[test]
    fn a_task_can_use_the_pool_it_runs_on() {
        for workers in SIZES {
            let pool = pool_with(workers);
            let total = AtomicUsize::new(0);
            let pool_ref = &pool;
            let total_ref = &total;
            let mut graph = TaskGraph::new();
            let a = graph.add("loop", move || {
                pool_ref.parallel_for(0..100, |i| {
                    total_ref.fetch_add(i, Ordering::Relaxed);
                });
            });
            graph
                .add_after("inner graph", &[a], move || {
                    let mut inner = TaskGraph::new();
                    let x = inner.add("x", move || {
                        total_ref.fetch_add(1_000, Ordering::Relaxed);
                    });
                    inner
                        .add_after("y", &[x], move || {
                            total_ref.fetch_add(10_000, Ordering::Relaxed);
                        })
                        .unwrap();
                    pool_ref.run_graph(&mut inner);
                })
                .unwrap();
            pool.run_graph(&mut graph);
            assert_eq!(
                total.load(Ordering::Relaxed),
                4_950 + 11_000,
                "{workers} workers"
            );
        }
    }

    #[test]
    fn a_job_can_run_a_graph_through_a_spawner() {
        for workers in SIZES {
            let pool = pool_with(workers);
            let spawner = pool.spawner();
            let handle = pool.spawn(move || {
                let sum = AtomicUsize::new(0);
                let sum_ref = &sum;
                let mut graph = TaskGraph::new();
                let a = graph.add("a", move || {
                    sum_ref.fetch_add(1, Ordering::SeqCst);
                });
                graph
                    .add_after("b", &[a], move || {
                        sum_ref.fetch_add(2, Ordering::SeqCst);
                    })
                    .unwrap();
                spawner.run_graph(&mut graph);
                drop(graph);
                sum.into_inner()
            });
            assert_eq!(handle.wait(), 3, "{workers} workers");
        }
    }

    #[test]
    fn independent_tasks_really_run_in_parallel() {
        // Two tasks that wait for each other can only both finish if they run at the same time.
        let pool = pool_with(3);
        let (to_a, from_b) = mpsc::channel::<()>();
        let (to_b, from_a) = mpsc::channel::<()>();
        let from_a = Mutex::new(from_a);
        let from_b = Mutex::new(from_b);
        let mut graph = TaskGraph::new();
        let (from_a, from_b) = (&from_a, &from_b);
        graph.add("a", move || {
            to_b.send(()).unwrap();
            from_b
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(30))
                .expect("b ran at the same time");
        });
        graph.add("b", move || {
            to_a.send(()).unwrap();
            from_a
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(30))
                .expect("a ran at the same time");
        });
        pool.run_graph(&mut graph);
    }

    #[test]
    fn building_errors_leave_the_graph_runnable() {
        let pool = pool_with(1);
        let count = AtomicUsize::new(0);
        let count = &count;
        let mut graph = TaskGraph::new();
        let a = graph.add("a", move || {
            count.fetch_add(1, Ordering::SeqCst);
        });
        let b = graph
            .add_after("b", &[a], move || {
                count.fetch_add(1, Ordering::SeqCst);
            })
            .unwrap();
        assert!(matches!(
            graph.depend(a, b),
            Err(ThreadingError::DependencyCycle { .. })
        ));
        pool.run_graph(&mut graph);
        assert_eq!(count.load(Ordering::SeqCst), 2);
    }
}
