//! The stack depth of parallel loops must not depend on how many chunks there are (milestone T7).
//!
//! Regression test for a stack overflow that the T7 benchmarks found: `parallel_chunks` over a million items with
//! a chunk size of 16 overflowed the stack of the calling thread. The calling thread, when it is not a worker,
//! helped while it waited by taking the *oldest* queued job (the shared queue is first-in first-out), which is
//! the biggest half of the range that was queued first; that job split again and waited again and helped again,
//! so the nesting grew with the number of chunks instead of with the logarithm of it.
//!
//! The loops below run on a thread with a small stack. A fixed bug shows up here as a clean pass; a regression as
//! a stack overflow, which aborts the whole test binary (that is the nature of a stack overflow).

use std::ops::Range;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use threading::{PoolConfig, ThreadPool, WorkerCount};

/// Small on purpose: the default main-thread stack is 8 MiB, which hides the problem for a long time.
const SMALL_STACK: usize = 512 * 1024;

const fn scaled(normal: usize, under_miri: usize) -> usize {
    if cfg!(miri) { under_miri } else { normal }
}

fn pool_with(workers: usize) -> ThreadPool {
    ThreadPool::new(PoolConfig::new().with_workers(WorkerCount::Exact(workers))).unwrap()
}

/// Runs `body` on a thread whose stack is `SMALL_STACK` bytes.
fn on_a_small_stack<R: Send + 'static>(body: impl FnOnce() -> R + Send + 'static) -> R {
    thread::Builder::new()
        .stack_size(SMALL_STACK)
        .spawn(body)
        .expect("the thread starts")
        .join()
        .expect("the body did not panic")
}

#[test]
fn many_tiny_chunks_do_not_deepen_the_stack_of_the_caller() {
    let items = scaled(200_000, 2_000);
    for workers in [0, 1, 3] {
        let visited = on_a_small_stack(move || {
            let pool = pool_with(workers);
            let visited = AtomicUsize::new(0);
            pool.parallel_chunks(0..items, 1, |range: Range<usize>| {
                visited.fetch_add(range.len(), Ordering::Relaxed);
            });
            visited.into_inner()
        });
        assert_eq!(visited, items, "{workers} workers");
    }
}

#[test]
fn the_other_loops_are_as_shallow_as_parallel_chunks() {
    let items = scaled(100_000, 1_000);
    for workers in [0, 2] {
        let sum = on_a_small_stack(move || {
            let pool = pool_with(workers);
            let mut data: Vec<usize> = (0..items).collect();
            pool.parallel_chunks_mut(&mut data, 1, |_offset, chunk: &mut [usize]| {
                for value in chunk {
                    *value += 1;
                }
            });
            pool.parallel_for_each(&data, |_| ());
            data.iter().sum::<usize>()
        });
        assert_eq!(sum, items * (items + 1) / 2, "{workers} workers");
    }
}

#[test]
fn a_deep_join_recursion_by_the_caller_itself_is_still_only_as_deep_as_the_recursion() {
    // `join` called recursively by user code: the depth is the depth of the user's recursion (here 12), never more.
    fn fib(pool: &ThreadPool, n: u32) -> u64 {
        if n < 2 {
            return u64::from(n);
        }
        let (a, b) = pool.join(|| fib(pool, n - 1), || fib(pool, n - 2));
        a + b
    }
    let n = scaled(20, 10) as u32;
    let expected = {
        let (mut a, mut b) = (0_u64, 1_u64);
        for _ in 0..n {
            (a, b) = (b, a + b);
        }
        a
    };
    for workers in [0, 2] {
        let result = on_a_small_stack(move || fib(&pool_with(workers), n));
        assert_eq!(result, expected, "{workers} workers");
    }
}
