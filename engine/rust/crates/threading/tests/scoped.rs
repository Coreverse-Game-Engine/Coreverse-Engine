//! Scoped jobs, `join` and the parallel loops (milestone T3), seen from outside the crate.

use std::collections::HashSet;
use std::ops::Range;
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::{Mutex, mpsc};
use std::thread;
use std::time::Duration;

use threading::{JobPriority, PoolConfig, Scope, ThreadPool, WorkerCount};

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

mod scope {
    use super::*;

    #[test]
    fn jobs_borrow_and_mutate_locals() {
        for workers in SIZES {
            let pool = pool_with(workers);
            let mut squares = vec![0_usize; 64];
            pool.scope(|scope| {
                for (n, slot) in squares.iter_mut().enumerate() {
                    scope.spawn(move || *slot = n * n);
                }
            });
            let expected: Vec<usize> = (0..64).map(|n| n * n).collect();
            assert_eq!(squares, expected, "{workers} workers");
        }
    }

    #[test]
    fn the_body_s_value_is_returned() {
        let pool = pool_with(2);
        let text = String::from("borrowed");
        let length = pool.scope(|scope| {
            scope.spawn(|| assert_eq!(text, "borrowed"));
            text.len() * 2
        });
        assert_eq!(length, 16);
    }

    #[test]
    fn the_scope_waits_for_every_job() {
        for workers in SIZES {
            let pool = pool_with(workers);
            let finished = AtomicUsize::new(0);
            pool.scope(|scope| {
                for _ in 0..12 {
                    scope.spawn(|| {
                        thread::sleep(Duration::from_millis(5));
                        finished.fetch_add(1, Ordering::SeqCst);
                    });
                }
                // The body returns at once; the jobs are far from done.
            });
            assert_eq!(finished.load(Ordering::SeqCst), 12, "{workers} workers");
        }
    }

    fn spawn_tree<'scope, 'env>(
        scope: &'scope Scope<'scope, 'env>,
        counter: &'env AtomicUsize,
        depth: u32,
    ) {
        counter.fetch_add(1, Ordering::SeqCst);
        if depth == 0 {
            return;
        }
        for _ in 0..2 {
            scope.spawn(move || spawn_tree(scope, counter, depth - 1));
        }
    }

    #[test]
    fn jobs_can_spawn_jobs_through_the_scope() {
        for workers in SIZES {
            let pool = pool_with(workers);
            let counter = AtomicUsize::new(0);
            let depth = scaled(8, 4) as u32;
            pool.scope(|scope| spawn_tree(scope, &counter, depth));
            assert_eq!(
                counter.load(Ordering::SeqCst),
                (1_usize << (depth + 1)) - 1,
                "{workers} workers"
            );
        }
    }

    #[test]
    fn what_a_job_captured_is_dropped_before_the_scope_returns() {
        for workers in SIZES {
            let pool = pool_with(workers);
            let drops = AtomicUsize::new(0);
            pool.scope(|scope| {
                for _ in 0..10 {
                    let guard = DropCounter(&drops);
                    scope.spawn(move || {
                        let _guard = guard;
                    });
                }
            });
            assert_eq!(drops.load(Ordering::SeqCst), 10, "{workers} workers");
        }
    }

    #[test]
    fn what_a_panicking_job_captured_is_dropped_too() {
        let pool = pool_with(2);
        let drops = AtomicUsize::new(0);
        let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
            pool.scope(|scope| {
                for _ in 0..5 {
                    let guard = DropCounter(&drops);
                    scope.spawn(move || {
                        let _guard = guard;
                        panic!("job failed");
                    });
                }
            });
        }));
        assert!(outcome.is_err());
        assert_eq!(drops.load(Ordering::SeqCst), 5);
    }

    #[test]
    fn a_job_panic_is_re_raised_after_every_job_finished() {
        for workers in SIZES {
            let pool = pool_with(workers);
            let finished = AtomicUsize::new(0);
            let finished = &finished;
            let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
                pool.scope(|scope| {
                    for n in 0..20 {
                        scope.spawn(move || {
                            thread::sleep(Duration::from_millis(2));
                            if n == 7 {
                                panic!("boom");
                            }
                            finished.fetch_add(1, Ordering::SeqCst);
                        });
                    }
                });
            }));
            let payload = outcome.expect_err("the scope re-raises the job's panic");
            assert_eq!(message(&*payload), Some("boom"), "{workers} workers");
            assert_eq!(
                finished.load(Ordering::SeqCst),
                19,
                "the other jobs all ran"
            );
            assert_eq!(pool.spawn(|| 1).wait(), 1, "the pool still works");
        }
    }

    #[test]
    fn a_body_panic_waits_for_the_jobs_and_wins_over_a_job_panic() {
        for workers in SIZES {
            let pool = pool_with(workers);
            let finished = AtomicUsize::new(0);
            let finished = &finished;
            let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
                pool.scope(|scope| {
                    for n in 0..8 {
                        scope.spawn(move || {
                            thread::sleep(Duration::from_millis(3));
                            finished.fetch_add(1, Ordering::SeqCst);
                            if n == 0 {
                                panic!("from a job");
                            }
                        });
                    }
                    panic!("from the body");
                });
            }));
            let payload = outcome.expect_err("the scope re-raises a panic");
            assert_eq!(
                message(&*payload),
                Some("from the body"),
                "{workers} workers"
            );
            assert_eq!(finished.load(Ordering::SeqCst), 8, "no job was left behind");
        }
    }

    #[test]
    fn a_scope_can_be_opened_inside_a_job_on_a_small_pool() {
        for workers in [1, 2] {
            let pool = pool_with(workers);
            let spawner = pool.spawner();
            let (report, reported) = mpsc::channel();
            drop(pool.spawn(move || {
                let mut parts = [0_u32; 6];
                spawner.scope(|scope| {
                    for (n, part) in parts.iter_mut().enumerate() {
                        scope.spawn(move || *part = n as u32 + 1);
                    }
                });
                report.send(parts.iter().sum::<u32>()).unwrap();
            }));
            let total = reported
                .recv_timeout(Duration::from_secs(20))
                .expect("no deadlock");
            assert_eq!(total, 21, "{workers} workers");
        }
    }

    #[test]
    fn priorities_apply_to_scoped_jobs() {
        // No workers: only the waiting thread runs jobs, so the order is exactly the queue order.
        let pool = pool_with(0);
        let order = Mutex::new(Vec::new());
        pool.scope(|scope| {
            scope.spawn_with_priority(JobPriority::Low, || order.lock().unwrap().push("low"));
            scope.spawn(|| order.lock().unwrap().push("normal"));
            scope.spawn_with_priority(JobPriority::High, || order.lock().unwrap().push("high"));
        });
        assert_eq!(*order.lock().unwrap(), ["high", "normal", "low"]);
    }

    #[test]
    fn many_jobs_in_one_scope() {
        let pool = pool_with(4);
        let counter = AtomicUsize::new(0);
        let jobs = scaled(10_000, 100);
        pool.scope(|scope| {
            for _ in 0..jobs {
                scope.spawn(|| {
                    counter.fetch_add(1, Ordering::Relaxed);
                });
            }
        });
        assert_eq!(counter.load(Ordering::Relaxed), jobs);
    }

    #[test]
    fn scopes_can_be_opened_over_and_over() {
        let pool = pool_with(3);
        let counter = AtomicUsize::new(0);
        let rounds = scaled(500, 20);
        for _ in 0..rounds {
            pool.scope(|scope| {
                scope.spawn(|| {
                    counter.fetch_add(1, Ordering::Relaxed);
                });
                scope.spawn(|| {
                    counter.fetch_add(1, Ordering::Relaxed);
                });
            });
        }
        assert_eq!(counter.load(Ordering::Relaxed), 2 * rounds);
    }

    #[test]
    fn a_scope_on_a_spawner_runs_inline_after_the_pool_has_shut_down() {
        let pool = pool_with(2);
        let spawner = pool.spawner();
        drop(pool);
        let mut values = [0_u32; 4];
        spawner.scope(|scope| {
            for (n, value) in values.iter_mut().enumerate() {
                scope.spawn(move || *value = n as u32);
            }
        });
        assert_eq!(values, [0, 1, 2, 3]);
    }
}

mod join {
    use super::*;

    /// Below this the recursion runs sequentially; above it every call is a `join`.
    const FIB_CUTOFF: u64 = if cfg!(miri) { 8 } else { 12 };
    const FIB_N: u64 = if cfg!(miri) { 16 } else { 27 };

    fn fib(pool: &ThreadPool, n: u64) -> u64 {
        if n < FIB_CUTOFF {
            return fib_sequential(n);
        }
        let (a, b) = pool.join(|| fib(pool, n - 1), || fib(pool, n - 2));
        a + b
    }

    fn fib_sequential(n: u64) -> u64 {
        if n < 2 {
            n
        } else {
            fib_sequential(n - 1) + fib_sequential(n - 2)
        }
    }

    #[test]
    fn both_results_come_back_and_both_may_borrow() {
        for workers in SIZES {
            let pool = pool_with(workers);
            let data = [1_u64, 2, 3, 4, 5, 6, 7, 8];
            let (left, right) = data.split_at(4);
            let (a, b) = pool.join(|| left.iter().sum::<u64>(), || right.iter().sum::<u64>());
            assert_eq!((a, b), (10, 26), "{workers} workers");
        }
    }

    #[test]
    fn a_recursive_join_computes_the_same_as_the_sequential_code() {
        for workers in SIZES {
            let pool = pool_with(workers);
            assert_eq!(
                fib(&pool, FIB_N),
                fib_sequential(FIB_N),
                "{workers} workers"
            );
        }
    }

    #[test]
    fn a_panic_in_a_waits_for_b_and_is_re_raised() {
        for workers in SIZES {
            let pool = pool_with(workers);
            let b_done = AtomicUsize::new(0);
            let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
                pool.join(
                    || -> u32 { panic!("a failed") },
                    || {
                        thread::sleep(Duration::from_millis(5));
                        b_done.fetch_add(1, Ordering::SeqCst);
                    },
                )
            }));
            let payload = outcome.expect_err("join re-raises");
            assert_eq!(message(&*payload), Some("a failed"), "{workers} workers");
            assert_eq!(b_done.load(Ordering::SeqCst), 1, "b ran to the end");
        }
    }

    #[test]
    fn a_panic_in_b_is_re_raised_after_a_finished() {
        for workers in SIZES {
            let pool = pool_with(workers);
            let a_done = AtomicUsize::new(0);
            let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
                pool.join(
                    || {
                        thread::sleep(Duration::from_millis(5));
                        a_done.fetch_add(1, Ordering::SeqCst);
                    },
                    || -> u32 { panic!("b failed") },
                )
            }));
            let payload = outcome.expect_err("join re-raises");
            assert_eq!(message(&*payload), Some("b failed"), "{workers} workers");
            assert_eq!(a_done.load(Ordering::SeqCst), 1);
        }
    }

    #[test]
    fn when_both_panic_a_wins() {
        let pool = pool_with(2);
        let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
            pool.join(|| -> u32 { panic!("a") }, || -> u32 { panic!("b") })
        }));
        let payload = outcome.expect_err("join re-raises");
        assert_eq!(message(&*payload), Some("a"));
    }

    #[test]
    fn many_small_joins_in_a_row() {
        let pool = pool_with(3);
        let mut total = 0_u64;
        let joins = scaled(2_000, 50) as u64;
        for n in 0..joins {
            let (a, b) = pool.join(|| n, || n * 2);
            total += a + b;
        }
        assert_eq!(total, (0..joins).map(|n| n * 3).sum());
    }
}

mod parallel_loops {
    use super::*;

    /// Visits `range` with `parallel_for` and returns how often each index in `0..len` was visited.
    fn visits(pool: &ThreadPool, len: usize, range: Range<usize>) -> Vec<u8> {
        let counts: Vec<AtomicU8> = (0..len).map(|_| AtomicU8::new(0)).collect();
        pool.parallel_for(range, |i| {
            counts[i].fetch_add(1, Ordering::SeqCst);
        });
        counts.iter().map(|c| c.load(Ordering::SeqCst)).collect()
    }

    #[test]
    fn parallel_for_visits_every_index_exactly_once() {
        for workers in SIZES {
            let pool = pool_with(workers);
            let len = scaled(10_000, 300);
            let seen = visits(&pool, len, 0..len);
            assert!(seen.iter().all(|&n| n == 1), "{workers} workers");
        }
    }

    #[test]
    fn parallel_for_handles_offsets_empty_reversed_and_tiny_ranges() {
        for workers in SIZES {
            let pool = pool_with(workers);
            let seen = visits(&pool, 400, 100..350);
            assert!(seen[..100].iter().all(|&n| n == 0), "{workers} workers");
            assert!(seen[100..350].iter().all(|&n| n == 1), "{workers} workers");
            assert!(seen[350..].iter().all(|&n| n == 0), "{workers} workers");

            assert!(visits(&pool, 10, 5..5).iter().all(|&n| n == 0), "empty");
            #[allow(clippy::reversed_empty_ranges)]
            let reversed = 7..3;
            assert!(
                visits(&pool, 10, reversed).iter().all(|&n| n == 0),
                "reversed"
            );
            assert_eq!(visits(&pool, 1, 0..1), [1], "one index");
            assert_eq!(visits(&pool, 2, 0..2), [1, 1], "two indices");
        }
    }

    #[test]
    fn parallel_chunks_cuts_exactly_like_slice_chunks() {
        for workers in SIZES {
            let pool = pool_with(workers);
            for (len, chunk_size) in [
                (10, 4),
                (10, 1),
                (10, 10),
                (10, 11),
                (12, 4),
                (1, 3),
                (0, 3),
                (scaled(1_000, 100), 7),
            ] {
                let seen = Mutex::new(Vec::new());
                pool.parallel_chunks(0..len, chunk_size, |chunk| seen.lock().unwrap().push(chunk));
                let mut seen = seen.into_inner().unwrap();
                seen.sort_by_key(|chunk| chunk.start);
                let expected: Vec<Range<usize>> = (0..len)
                    .step_by(chunk_size)
                    .map(|start| start..(start + chunk_size).min(len))
                    .collect();
                assert_eq!(
                    seen, expected,
                    "{workers} workers, len {len}, chunk {chunk_size}"
                );
            }
        }
    }

    #[test]
    fn parallel_chunks_respects_a_range_that_does_not_start_at_zero() {
        let pool = pool_with(3);
        let seen = Mutex::new(Vec::new());
        pool.parallel_chunks(5..17, 5, |chunk| seen.lock().unwrap().push(chunk));
        let mut seen = seen.into_inner().unwrap();
        seen.sort_by_key(|chunk| chunk.start);
        assert_eq!(seen, [5..10, 10..15, 15..17]);
    }

    #[test]
    #[should_panic(expected = "chunk size")]
    fn a_chunk_size_of_zero_is_rejected_for_ranges() {
        pool_with(0).parallel_chunks(0..10, 0, |_| ());
    }

    #[test]
    #[should_panic(expected = "chunk size")]
    fn a_chunk_size_of_zero_is_rejected_for_slices() {
        pool_with(0).parallel_chunks_mut(&mut [1, 2, 3], 0, |_, _| ());
    }

    #[test]
    fn parallel_for_each_visits_every_item() {
        for workers in SIZES {
            let pool = pool_with(workers);
            let count = scaled(1_000, 100) as u64;
            let values: Vec<u64> = (1..=count).collect();
            let total = AtomicUsize::new(0);
            pool.parallel_for_each(&values, |value| {
                total.fetch_add(*value as usize, Ordering::Relaxed);
            });
            assert_eq!(
                total.load(Ordering::Relaxed) as u64,
                count * (count + 1) / 2,
                "{workers} workers"
            );
            pool.parallel_for_each(&Vec::<u64>::new(), |_| panic!("no items"));
        }
    }

    #[test]
    fn parallel_for_each_mut_changes_every_item_once() {
        for workers in SIZES {
            let pool = pool_with(workers);
            let mut values = vec![0_u32; scaled(5_000, 200)];
            pool.parallel_for_each_mut(&mut values, |value| *value += 1);
            assert!(values.iter().all(|&v| v == 1), "{workers} workers");
            pool.parallel_for_each_mut(&mut Vec::<u32>::new(), |_| panic!("no items"));
        }
    }

    #[test]
    fn parallel_chunks_mut_passes_the_start_offset_and_disjoint_chunks() {
        for workers in SIZES {
            let pool = pool_with(workers);
            for (len, chunk_size) in [(10, 4), (100, 7), (64, 64), (5, 9), (1, 1)] {
                let mut values = vec![usize::MAX; len];
                let lengths = Mutex::new(Vec::new());
                pool.parallel_chunks_mut(&mut values, chunk_size, |start, chunk| {
                    lengths.lock().unwrap().push(chunk.len());
                    for (offset, value) in chunk.iter_mut().enumerate() {
                        *value = start + offset;
                    }
                });
                let expected: Vec<usize> = (0..len).collect();
                assert_eq!(
                    values, expected,
                    "{workers} workers, len {len}, chunk {chunk_size}"
                );
                assert!(
                    lengths
                        .into_inner()
                        .unwrap()
                        .iter()
                        .all(|&n| n <= chunk_size)
                );
            }
        }
    }

    #[test]
    fn a_panic_in_the_body_is_re_raised_and_the_pool_survives() {
        for workers in SIZES {
            let pool = pool_with(workers);
            let visited = AtomicUsize::new(0);
            let len = scaled(2_000, 200);
            let bad = scaled(1_234, 123);
            let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
                pool.parallel_for(0..len, |i| {
                    visited.fetch_add(1, Ordering::SeqCst);
                    if i == bad {
                        panic!("index {bad}");
                    }
                });
            }));
            let payload = outcome.expect_err("the loop re-raises");
            assert_eq!(
                message(&*payload),
                Some(format!("index {bad}").as_str()),
                "{workers} workers"
            );
            assert!(visited.load(Ordering::SeqCst) > 1, "other chunks still ran");
            assert_eq!(pool.spawn(|| 5).wait(), 5);
        }
    }

    #[test]
    fn a_loop_can_run_inside_a_job_through_a_spawner() {
        let len = scaled(1_000, 100);
        for workers in [1, 2] {
            let pool = pool_with(workers);
            let spawner = pool.spawner();
            let (report, reported) = mpsc::channel();
            drop(pool.spawn(move || {
                let total = AtomicUsize::new(0);
                spawner.parallel_for(0..len, |i| {
                    total.fetch_add(i, Ordering::Relaxed);
                });
                report.send(total.into_inner()).unwrap();
            }));
            let total = reported
                .recv_timeout(Duration::from_secs(20))
                .expect("no deadlock");
            assert_eq!(total, len * (len - 1) / 2, "{workers} workers");
        }
    }

    #[test]
    fn loops_can_be_nested() {
        for workers in [0, 1, 2, 4] {
            let pool = pool_with(workers);
            let total = AtomicUsize::new(0);
            let (outer, inner) = (scaled(20, 4), scaled(30, 5));
            pool.parallel_for(0..outer, |_| {
                pool.parallel_for(0..inner, |_| {
                    total.fetch_add(1, Ordering::Relaxed);
                });
            });
            assert_eq!(
                total.load(Ordering::Relaxed),
                outer * inner,
                "{workers} workers"
            );
        }
    }

    #[test]
    fn the_loops_work_through_a_spawner_after_the_pool_shut_down() {
        let pool = pool_with(2);
        let spawner = pool.spawner();
        drop(pool);
        let mut values = vec![1_u32; 100];
        spawner.parallel_for_each_mut(&mut values, |v| *v += 1);
        assert!(values.iter().all(|&v| v == 2));
        let total = AtomicUsize::new(0);
        spawner.parallel_for(0..100, |i| {
            total.fetch_add(i, Ordering::Relaxed);
        });
        assert_eq!(total.into_inner(), 4_950);
    }

    #[test]
    fn a_parallel_loop_really_uses_several_threads() {
        let pool = pool_with(4);
        let threads = Mutex::new(HashSet::new());
        pool.parallel_chunks(0..16, 1, |_| {
            // Sleeping lets the other threads find and steal the remaining chunks, even on one CPU core.
            thread::sleep(Duration::from_millis(10));
            threads.lock().unwrap().insert(thread::current().id());
        });
        assert!(threads.into_inner().unwrap().len() >= 2);
    }

    #[test]
    fn the_result_matches_the_sequential_computation() {
        fn is_prime(n: u32) -> bool {
            n >= 2
                && (2..)
                    .take_while(|d| d * d <= n)
                    .all(|d| !n.is_multiple_of(d))
        }
        let limit = scaled(20_000, 1_500) as u32;
        let expected = (0..limit).filter(|&n| is_prime(n)).count();
        for workers in SIZES {
            let pool = pool_with(workers);
            let found = AtomicUsize::new(0);
            pool.parallel_chunks(0..limit as usize, scaled(500, 100), |chunk| {
                let count = chunk.filter(|&n| is_prime(n as u32)).count();
                found.fetch_add(count, Ordering::Relaxed);
            });
            assert_eq!(found.load(Ordering::Relaxed), expected, "{workers} workers");
        }
    }

    #[test]
    fn loops_can_be_run_over_and_over() {
        let pool = pool_with(3);
        let total = AtomicUsize::new(0);
        let (rounds, per_round) = (scaled(200, 10), scaled(50, 20));
        for _ in 0..rounds {
            pool.parallel_for(0..per_round, |_| {
                total.fetch_add(1, Ordering::Relaxed);
            });
        }
        assert_eq!(total.load(Ordering::Relaxed), rounds * per_round);
    }
}
