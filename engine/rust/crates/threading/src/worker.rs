//! Which pool and worker the calling thread belongs to, if any.

use std::cell::{Cell, RefCell};

thread_local! {
    /// `(pool id, worker index)` of the calling thread, set once when a worker starts.
    static CURRENT: Cell<Option<(usize, usize)>> = const { Cell::new(None) };

    /// The ids of the pools whose jobs the calling thread is running right now. A stack, because jobs nest
    /// when a thread that waits for a job runs other jobs meanwhile (see `JobHandle`).
    static RUNNING: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
}

/// Marks the calling thread as running a job of a pool, until the guard is dropped.
#[must_use = "the thread counts as running the job only while the guard lives"]
pub(crate) struct RunningJob(());

/// Marks the calling thread as running a job of pool `pool`. Used around every job the pool's code runs, on
/// workers and on every thread that helps (a waiter, the thread that shuts the pool down).
pub(crate) fn enter_job(pool: usize) -> RunningJob {
    RUNNING.with(|running| running.borrow_mut().push(pool));
    RunningJob(())
}

impl Drop for RunningJob {
    fn drop(&mut self) {
        // `try_with`: this can run while the thread is being torn down, when the slot is already gone.
        let _ = RUNNING.try_with(|running| running.borrow_mut().pop());
    }
}

/// How many jobs (of any pool) the calling thread is running at this moment: 0 outside of jobs, more than 1
/// while a job runs jobs inside itself (help while waiting).
pub(crate) fn running_depth() -> usize {
    RUNNING.with(|running| running.borrow().len())
}

/// Whether the calling thread is running a job of pool `pool` right now.
///
/// After a pool has begun to shut down, only such a thread may still add work to it: the jobs a running job
/// spawns belong to the work the shutdown has to finish, whereas a thread that merely holds a spawner must
/// not be able to keep a pool from ever finishing.
pub(crate) fn is_running_job_of(pool: usize) -> bool {
    RUNNING.with(|running| running.borrow().contains(&pool))
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
    fn a_thread_counts_as_running_a_job_only_while_the_guard_lives() {
        assert!(!is_running_job_of(5));
        let outer = enter_job(5);
        assert!(is_running_job_of(5));
        assert!(
            !is_running_job_of(6),
            "another pool's job is not this pool's"
        );
        {
            // Helping while waiting: a job of another pool runs inside the first one.
            let _inner = enter_job(6);
            assert!(is_running_job_of(5) && is_running_job_of(6));
        }
        assert!(is_running_job_of(5) && !is_running_job_of(6));
        drop(outer);
        assert!(!is_running_job_of(5));
    }

    #[test]
    fn the_same_pool_can_be_entered_twice() {
        let first = enter_job(9);
        let second = enter_job(9);
        drop(second);
        assert!(is_running_job_of(9), "the outer job is still running");
        drop(first);
        assert!(!is_running_job_of(9));
    }

    #[test]
    fn running_jobs_are_tracked_per_thread() {
        let _here = enter_job(11);
        std::thread::spawn(|| assert!(!is_running_job_of(11)))
            .join()
            .unwrap();
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
