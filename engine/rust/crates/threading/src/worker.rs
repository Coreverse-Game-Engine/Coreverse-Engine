//! Which pool and worker the calling thread belongs to, if any.

use std::cell::Cell;

thread_local! {
    /// `(pool id, worker index)` of the calling thread, set once when a worker starts.
    static CURRENT: Cell<Option<(usize, usize)>> = const { Cell::new(None) };
}

/// Marks the calling thread as worker `index` of pool `pool`.
pub(crate) fn enter(pool: usize, index: usize) {
    CURRENT.with(|current| current.set(Some((pool, index))));
}

/// The index of the calling thread among the workers of pool `pool`; `None` for every other thread, including
/// the workers of other pools.
pub(crate) fn index_in(pool: usize) -> Option<usize> {
    CURRENT
        .with(Cell::get)
        .and_then(|(id, index)| (id == pool).then_some(index))
}

/// The index of the calling thread among the workers of the pool it belongs to (`0` for the first worker, the
/// one named `<prefix>-0`), or `None` when the calling thread is not a pool worker.
///
/// Useful for per-worker data (scratch buffers, statistics) and for assertions that code runs, or does not
/// run, on a worker. Keep in mind that a thread that waits for a job may run other jobs itself (see
/// [`JobHandle`](crate::JobHandle)); such a job sees the waiting thread's answer, which is `None` for a thread
/// that is not a worker.
#[must_use]
pub fn worker_index() -> Option<usize> {
    CURRENT.with(Cell::get).map(|(_, index)| index)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_thread_is_not_a_worker() {
        assert_eq!(worker_index(), None);
        assert_eq!(index_in(0), None);
    }

    #[test]
    fn a_marked_thread_is_a_worker_of_its_own_pool_only() {
        std::thread::spawn(|| {
            enter(7, 3);
            assert_eq!(worker_index(), Some(3));
            assert_eq!(index_in(7), Some(3));
            assert_eq!(index_in(8), None, "another pool's id does not match");
        })
        .join()
        .unwrap();
    }
}
