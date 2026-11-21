//! Small helpers around the standard synchronization types.

use std::time::Duration;

/// Locks a mutex and ignores poisoning (see [`utilities::sync`]).
///
/// Job code never runs while one of this crate's locks is held (jobs are wrapped in `catch_unwind` and run
/// outside every lock), so a poisoned lock can only come from a bug in this crate's own few lines under the
/// lock. Those lines leave the protected data consistent at every point where they could panic, so taking the
/// data back is safe and keeps one failure from taking the whole pool down.
pub(crate) use utilities::sync::lock;

/// How long a thread that waits on a pool *without workers* sleeps before it looks at the queues again.
///
/// A waiting thread normally sleeps until the job it waits for finishes, because the queues are empty and the
/// job must be running somewhere. That holds only while a worker would run whatever the finishing thread queues
/// next (a continuation, a task whose dependencies just finished). A pool without workers has none, so the
/// thread that queued the work might be gone and nobody else would ever run it: such a waiter wakes up every
/// `IDLE_POLL` to look for work itself. A single thread driving the pool never gets here (it finds its jobs in
/// the queue); the poll only costs anything while another thread is running a job the waiter needs.
pub(crate) const IDLE_POLL: Duration = Duration::from_millis(1);
