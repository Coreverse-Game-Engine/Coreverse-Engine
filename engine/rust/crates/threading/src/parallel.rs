//! `scope`, `join` and the parallel loops, on top of [`scope::run_scope`](crate::scope).
//!
//! Everything here is safe code. The loops split their range (or slice) in halves with [`join`]: one half runs
//! on the calling thread, the other is queued and, if nobody has started it when the first half is done, taken
//! back and run by the caller itself (see [`join`] for why that must not be left to "help while waiting").
//! Chunk boundaries are fixed by the chunk size the caller passes, like `slice::chunks`, not by how the work
//! happens to be split.

use std::ops::Range;
use std::sync::Arc;

use crate::pool::{Shared, Spawner, ThreadPool};
use crate::priority::JobPriority;
use crate::scope::{Scope, run_scope};

/// How many chunks per thread the automatic chunk size aims for: enough that a slow chunk does not leave the
/// other threads idle at the end, few enough that the per-chunk cost stays small.
const CHUNKS_PER_THREAD: usize = 4;

/// The chunk size the loops without an explicit one use for `len` items on a pool with `workers` workers. The
/// calling thread takes part, so there are `workers + 1` threads; with no workers there is one chunk.
fn auto_chunk_size(len: usize, workers: usize) -> usize {
    if workers == 0 {
        return len.max(1);
    }
    len.div_ceil((workers + 1) * CHUNKS_PER_THREAD).max(1)
}

/// Runs `a` on this thread and `b` as a scoped job, and returns both results.
///
/// `b` is queued so that another thread can steal it while `a` runs. If nobody has started it by the time `a`
/// is done, this thread takes it back and runs it itself ([`Claim::run_here`](crate::scope)). That is what keeps
/// the stack shallow: the work stays depth-first, as in a sequential recursion, and a thread waits (and helps)
/// only when `b` really is running on another thread. Leaving it to "help while waiting" overflowed the stack of
/// a caller that was not a worker (a million items in chunks of 16): the oldest queued job is the biggest piece
/// of some enclosing split, and running it inside this wait nests another round of splits inside this one, so
/// the depth grew with the number of pieces instead of with the depth of the recursion.
pub(crate) fn join<A, B, RA, RB>(shared: &Arc<Shared>, a: A, b: B) -> (RA, RB)
where
    A: FnOnce() -> RA + Send,
    B: FnOnce() -> RB + Send,
    RA: Send,
    RB: Send,
{
    let mut b_result: Option<RB> = None;
    let a_result = run_scope(shared, |scope| {
        let claim = scope.spawn_claimable(JobPriority::Normal, || b_result = Some(b()));
        // If `a` panics we do not get here: the panic leaves the scope body, the scope waits for the queued
        // job (which then runs `b`) and re-raises the panic, so `b` always runs to the end, as before.
        let a_result = a();
        claim.run_here();
        a_result
    });
    match b_result {
        Some(b_result) => (a_result, b_result),
        None => {
            unreachable!("the scope returned normally, so `b` ran to the end without panicking")
        }
    }
}

/// Calls `f` once for every chunk of `range` (`chunk` indices each, the last one possibly shorter).
fn chunks_of_range<F>(shared: &Arc<Shared>, range: Range<usize>, chunk: usize, f: &F)
where
    F: Fn(Range<usize>) + Sync,
{
    let len = range.end.saturating_sub(range.start);
    if len == 0 {
        return;
    }
    if len <= chunk {
        f(range);
        return;
    }
    // Split on a chunk boundary, so the pieces are made of whole chunks.
    let mid = range.start + (len.div_ceil(chunk) / 2) * chunk;
    join(
        shared,
        || chunks_of_range(shared, range.start..mid, chunk, f),
        || chunks_of_range(shared, mid..range.end, chunk, f),
    );
}

/// Calls `f(offset, chunk)` for every chunk of `slice`; `offset` is the index of the chunk's first item.
fn chunks_of_slice<T, F>(shared: &Arc<Shared>, slice: &[T], offset: usize, chunk: usize, f: &F)
where
    T: Sync,
    F: Fn(usize, &[T]) + Sync,
{
    if slice.is_empty() {
        return;
    }
    if slice.len() <= chunk {
        f(offset, slice);
        return;
    }
    let mid = (slice.len().div_ceil(chunk) / 2) * chunk;
    let (left, right) = slice.split_at(mid);
    join(
        shared,
        || chunks_of_slice(shared, left, offset, chunk, f),
        || chunks_of_slice(shared, right, offset + mid, chunk, f),
    );
}

/// The same for mutable chunks: the halves are disjoint (`split_at_mut`), so no two jobs share an item.
fn chunks_of_slice_mut<T, F>(
    shared: &Arc<Shared>,
    slice: &mut [T],
    offset: usize,
    chunk: usize,
    f: &F,
) where
    T: Send,
    F: Fn(usize, &mut [T]) + Sync,
{
    if slice.is_empty() {
        return;
    }
    if slice.len() <= chunk {
        f(offset, slice);
        return;
    }
    let mid = (slice.len().div_ceil(chunk) / 2) * chunk;
    let (left, right) = slice.split_at_mut(mid);
    join(
        shared,
        || chunks_of_slice_mut(shared, left, offset, chunk, f),
        || chunks_of_slice_mut(shared, right, offset + mid, chunk, f),
    );
}

fn assert_chunk_size(chunk_size: usize) {
    assert!(chunk_size > 0, "the chunk size must be at least 1");
}

fn parallel_for_impl<F>(shared: &Arc<Shared>, range: Range<usize>, f: &F)
where
    F: Fn(usize) + Sync,
{
    let len = range.end.saturating_sub(range.start);
    let chunk = auto_chunk_size(len, shared.worker_count());
    chunks_of_range(shared, range, chunk, &|chunk_range: Range<usize>| {
        for index in chunk_range {
            f(index);
        }
    });
}

fn parallel_for_each_impl<T, F>(shared: &Arc<Shared>, items: &[T], f: &F)
where
    T: Sync,
    F: Fn(&T) + Sync,
{
    let chunk = auto_chunk_size(items.len(), shared.worker_count());
    chunks_of_slice(shared, items, 0, chunk, &|_, part: &[T]| {
        for item in part {
            f(item);
        }
    });
}

fn parallel_for_each_mut_impl<T, F>(shared: &Arc<Shared>, items: &mut [T], f: &F)
where
    T: Send,
    F: Fn(&mut T) + Sync,
{
    let chunk = auto_chunk_size(items.len(), shared.worker_count());
    chunks_of_slice_mut(shared, items, 0, chunk, &|_, part: &mut [T]| {
        for item in part {
            f(item);
        }
    });
}

impl ThreadPool {
    /// Runs `body` with a [`Scope`] whose jobs may borrow from the surrounding code, waits for every job
    /// spawned in it, and returns the body's value.
    ///
    /// The calling thread does not sit idle while it waits: it runs queued jobs, so a scope works on a pool
    /// with no workers too. A scope may be opened from inside a job.
    ///
    /// # Panics
    ///
    /// If the body panics, all spawned jobs are still waited for, then the body's panic is re-raised. If a job
    /// panics (and the body does not), the other jobs still run to the end, then the first job panic is
    /// re-raised. Nothing is cancelled.
    ///
    /// ```
    /// use threading::{PoolConfig, ThreadPool};
    ///
    /// let pool = ThreadPool::new(PoolConfig::new())?;
    /// let mut squares = vec![0_u64; 8];
    /// pool.scope(|scope| {
    ///     for (n, slot) in squares.iter_mut().enumerate() {
    ///         // Each job borrows its own element of `squares`; no `Arc`, no `'static`.
    ///         scope.spawn(move || *slot = (n * n) as u64);
    ///     }
    /// });
    /// assert_eq!(squares, [0, 1, 4, 9, 16, 25, 36, 49]);
    /// # Ok::<(), threading::ThreadingError>(())
    /// ```
    pub fn scope<'env, F, T>(&self, body: F) -> T
    where
        F: for<'scope> FnOnce(&'scope Scope<'scope, 'env>) -> T,
    {
        run_scope(&self.shared, body)
    }

    /// Runs `a` on the calling thread and `b` as a job, possibly on another thread, and returns both results.
    ///
    /// Both closures may borrow from the surrounding code. If one of them panics the other still runs to the
    /// end; then the panic is re-raised (`a`'s first when both panic).
    ///
    /// ```
    /// use threading::{PoolConfig, ThreadPool};
    ///
    /// let pool = ThreadPool::new(PoolConfig::new())?;
    /// let data = [1_u64, 2, 3, 4, 5, 6];
    /// let (left, right) = data.split_at(3);
    /// let (a, b) = pool.join(|| left.iter().sum::<u64>(), || right.iter().sum::<u64>());
    /// assert_eq!((a, b), (6, 15));
    /// # Ok::<(), threading::ThreadingError>(())
    /// ```
    pub fn join<A, B, RA, RB>(&self, a: A, b: B) -> (RA, RB)
    where
        A: FnOnce() -> RA + Send,
        B: FnOnce() -> RB + Send,
        RA: Send,
        RB: Send,
    {
        join(&self.shared, a, b)
    }

    /// Calls `f(index)` for every index in `range`, spread over the workers and the calling thread, and returns
    /// when all calls are done. The order of the calls is unspecified; each index is visited exactly once.
    ///
    /// The range is cut into chunks (about four per thread) that run in turn on one thread each; use
    /// [`parallel_chunks`](Self::parallel_chunks) to choose the chunk size yourself. Does nothing for an empty
    /// range. If `f` panics the other calls still run, then the first panic is re-raised.
    ///
    /// ```
    /// use std::sync::atomic::{AtomicU64, Ordering};
    /// use threading::{PoolConfig, ThreadPool};
    ///
    /// let pool = ThreadPool::new(PoolConfig::new())?;
    /// let total = AtomicU64::new(0);
    /// pool.parallel_for(0..1_000, |i| {
    ///     total.fetch_add(i as u64, Ordering::Relaxed);
    /// });
    /// assert_eq!(total.into_inner(), 499_500);
    /// # Ok::<(), threading::ThreadingError>(())
    /// ```
    pub fn parallel_for<F>(&self, range: Range<usize>, f: F)
    where
        F: Fn(usize) + Sync,
    {
        parallel_for_impl(&self.shared, range, &f);
    }

    /// Cuts `range` into chunks of `chunk_size` indices (the last one may be shorter), exactly like
    /// `slice::chunks`, and calls `f(chunk)` for each chunk, spread over the workers and the calling thread.
    /// The chunk boundaries do not depend on how the work is scheduled. The order of the calls is unspecified.
    ///
    /// Choose the chunk size so that one chunk is worth a job (some microseconds of work at least).
    ///
    /// # Panics
    ///
    /// Panics if `chunk_size` is 0, and re-raises the first panic of `f` after all chunks ran.
    ///
    /// ```
    /// use std::sync::Mutex;
    /// use threading::{PoolConfig, ThreadPool};
    ///
    /// let pool = ThreadPool::new(PoolConfig::new())?;
    /// let seen = Mutex::new(Vec::new());
    /// pool.parallel_chunks(0..10, 4, |chunk| seen.lock().unwrap().push(chunk));
    /// let mut seen = seen.into_inner().unwrap();
    /// seen.sort_by_key(|chunk| chunk.start);
    /// assert_eq!(seen, [0..4, 4..8, 8..10]);
    /// # Ok::<(), threading::ThreadingError>(())
    /// ```
    pub fn parallel_chunks<F>(&self, range: Range<usize>, chunk_size: usize, f: F)
    where
        F: Fn(Range<usize>) + Sync,
    {
        assert_chunk_size(chunk_size);
        chunks_of_range(&self.shared, range, chunk_size, &f);
    }

    /// Calls `f(&item)` for every item of `items`, spread over the workers and the calling thread. Chunking as
    /// in [`parallel_for`](Self::parallel_for).
    ///
    /// ```
    /// use std::sync::atomic::{AtomicU64, Ordering};
    /// use threading::{PoolConfig, ThreadPool};
    ///
    /// let pool = ThreadPool::new(PoolConfig::new())?;
    /// let values: Vec<u64> = (1..=100).collect();
    /// let total = AtomicU64::new(0);
    /// pool.parallel_for_each(&values, |value| {
    ///     total.fetch_add(*value, Ordering::Relaxed);
    /// });
    /// assert_eq!(total.into_inner(), 5_050);
    /// # Ok::<(), threading::ThreadingError>(())
    /// ```
    pub fn parallel_for_each<T, F>(&self, items: &[T], f: F)
    where
        T: Sync,
        F: Fn(&T) + Sync,
    {
        parallel_for_each_impl(&self.shared, items, &f);
    }

    /// Calls `f(&mut item)` for every item of `items`, spread over the workers and the calling thread. The
    /// slice is split into disjoint parts, so no two calls see the same item. Chunking as in
    /// [`parallel_for`](Self::parallel_for).
    ///
    /// ```
    /// use threading::{PoolConfig, ThreadPool};
    ///
    /// let pool = ThreadPool::new(PoolConfig::new())?;
    /// let mut values: Vec<u32> = (0..100).collect();
    /// pool.parallel_for_each_mut(&mut values, |value| *value *= 2);
    /// assert_eq!(values[99], 198);
    /// # Ok::<(), threading::ThreadingError>(())
    /// ```
    pub fn parallel_for_each_mut<T, F>(&self, items: &mut [T], f: F)
    where
        T: Send,
        F: Fn(&mut T) + Sync,
    {
        parallel_for_each_mut_impl(&self.shared, items, &f);
    }

    /// Cuts `items` into chunks of `chunk_size` items (the last one may be shorter), like `slice::chunks_mut`,
    /// and calls `f(start, chunk)` for each, where `start` is the index of the chunk's first item in `items`.
    /// The chunks are disjoint, so the calls may run at the same time.
    ///
    /// # Panics
    ///
    /// Panics if `chunk_size` is 0, and re-raises the first panic of `f` after all chunks ran.
    ///
    /// ```
    /// use threading::{PoolConfig, ThreadPool};
    ///
    /// let pool = ThreadPool::new(PoolConfig::new())?;
    /// let mut values = vec![0_usize; 10];
    /// pool.parallel_chunks_mut(&mut values, 4, |start, chunk| {
    ///     for (offset, value) in chunk.iter_mut().enumerate() {
    ///         *value = start + offset;
    ///     }
    /// });
    /// assert_eq!(values, [0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
    /// # Ok::<(), threading::ThreadingError>(())
    /// ```
    pub fn parallel_chunks_mut<T, F>(&self, items: &mut [T], chunk_size: usize, f: F)
    where
        T: Send,
        F: Fn(usize, &mut [T]) + Sync,
    {
        assert_chunk_size(chunk_size);
        chunks_of_slice_mut(&self.shared, items, 0, chunk_size, &f);
    }
}

/// The same operations as on [`ThreadPool`], for code that holds a spawner (typically a job that wants to
/// parallelize part of its own work). They work after the pool has shut down too, then on the calling thread
/// alone.
impl Spawner {
    /// See [`ThreadPool::scope`].
    pub fn scope<'env, F, T>(&self, body: F) -> T
    where
        F: for<'scope> FnOnce(&'scope Scope<'scope, 'env>) -> T,
    {
        run_scope(&self.shared, body)
    }

    /// See [`ThreadPool::join`].
    pub fn join<A, B, RA, RB>(&self, a: A, b: B) -> (RA, RB)
    where
        A: FnOnce() -> RA + Send,
        B: FnOnce() -> RB + Send,
        RA: Send,
        RB: Send,
    {
        join(&self.shared, a, b)
    }

    /// See [`ThreadPool::parallel_for`].
    pub fn parallel_for<F>(&self, range: Range<usize>, f: F)
    where
        F: Fn(usize) + Sync,
    {
        parallel_for_impl(&self.shared, range, &f);
    }

    /// See [`ThreadPool::parallel_chunks`].
    ///
    /// # Panics
    ///
    /// Panics if `chunk_size` is 0.
    pub fn parallel_chunks<F>(&self, range: Range<usize>, chunk_size: usize, f: F)
    where
        F: Fn(Range<usize>) + Sync,
    {
        assert_chunk_size(chunk_size);
        chunks_of_range(&self.shared, range, chunk_size, &f);
    }

    /// See [`ThreadPool::parallel_for_each`].
    pub fn parallel_for_each<T, F>(&self, items: &[T], f: F)
    where
        T: Sync,
        F: Fn(&T) + Sync,
    {
        parallel_for_each_impl(&self.shared, items, &f);
    }

    /// See [`ThreadPool::parallel_for_each_mut`].
    pub fn parallel_for_each_mut<T, F>(&self, items: &mut [T], f: F)
    where
        T: Send,
        F: Fn(&mut T) + Sync,
    {
        parallel_for_each_mut_impl(&self.shared, items, &f);
    }

    /// See [`ThreadPool::parallel_chunks_mut`].
    ///
    /// # Panics
    ///
    /// Panics if `chunk_size` is 0.
    pub fn parallel_chunks_mut<T, F>(&self, items: &mut [T], chunk_size: usize, f: F)
    where
        T: Send,
        F: Fn(usize, &mut [T]) + Sync,
    {
        assert_chunk_size(chunk_size);
        chunks_of_slice_mut(&self.shared, items, 0, chunk_size, &f);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_automatic_chunk_size_is_sensible() {
        assert_eq!(auto_chunk_size(0, 4), 1, "never zero");
        assert_eq!(auto_chunk_size(1_000, 0), 1_000, "no workers: one chunk");
        assert_eq!(
            auto_chunk_size(1_000, 3),
            63,
            "about four chunks per thread of four"
        );
        assert_eq!(auto_chunk_size(3, 7), 1, "tiny ranges: one item per chunk");
    }
}
