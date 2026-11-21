//! Observing a pool: hooks for a profiler and per-worker statistics.
//!
//! Two mechanisms, for two kinds of reader:
//!
//! * a [`JobObserver`] is called **as things happen** (a job starts, a job finishes, a job is stolen, a worker
//!   goes to sleep and wakes up). A profiler implements it to draw a timeline. This crate does not depend on
//!   any profiler: the hooks are a trait, and whoever owns the profiler implements it and installs it with
//!   [`PoolConfig::with_observer`](crate::PoolConfig::with_observer).
//! * [`PoolStats`] are **counters**, always kept, that can be read at any time with
//!   [`ThreadPool::stats`](crate::ThreadPool::stats): how many jobs each worker ran, stole, how long it was busy
//!   or asleep. A debug overlay or a test reads them.
//!
//! Without an observer and without timing the cost of both is a handful of relaxed atomic additions per job, each
//! on a cache line of its own worker.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::priority::JobPriority;

/// Tells pools apart: unique per pool within the process, never reused. Observers that serve several pools (the
/// compute pool and the IO pool) use it to know whose event they got; [`ThreadPool::id`](crate::ThreadPool::id)
/// gives the id of a pool.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct PoolId(pub(crate) usize);

impl PoolId {
    /// The number behind the id.
    #[must_use]
    pub fn get(self) -> usize {
        self.0
    }
}

impl fmt::Display for PoolId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "pool#{}", self.0)
    }
}

/// A job is about to run.
#[derive(Copy, Clone, Debug)]
#[non_exhaustive]
pub struct JobStart {
    /// The pool the job belongs to.
    pub pool: PoolId,
    /// Numbers the jobs a pool runs, from 0 in the order they start. Matches the [`JobFinish`] of the same job.
    /// (The numbering is only kept while an observer or timing is on.)
    pub job: u64,
    /// The worker that runs it, or `None` when a thread that is not a worker of the pool runs it (a thread that
    /// waits for a job, the thread that shuts the pool down, any thread in a pool without workers).
    pub worker: Option<usize>,
    /// The priority the job was queued with.
    pub priority: JobPriority,
    /// When it started.
    pub at: Instant,
}

/// A job has finished running.
///
/// A job that panicked is reported like any other: the panic is delivered through its handle, not here.
#[derive(Copy, Clone, Debug)]
#[non_exhaustive]
pub struct JobFinish {
    /// The pool the job belongs to.
    pub pool: PoolId,
    /// The number given in the matching [`JobStart`].
    pub job: u64,
    /// The thread that ran it, as in [`JobStart::worker`].
    pub worker: Option<usize>,
    /// The priority the job was queued with.
    pub priority: JobPriority,
    /// When it started.
    pub started: Instant,
    /// How long it ran. A job that waits for another job and runs jobs meanwhile (help while waiting) counts
    /// the time of those jobs too, so nested jobs are *inside* the span of the job that ran them.
    pub duration: Duration,
}

/// A worker, or a helping thread, took a job from the queue of another worker.
#[derive(Copy, Clone, Debug)]
#[non_exhaustive]
pub struct JobSteal {
    /// The pool.
    pub pool: PoolId,
    /// The worker that took the job, or `None` for a thread that is not one of the pool's workers.
    pub thief: Option<usize>,
    /// The worker whose queue the job was in.
    pub victim: usize,
    /// The priority of the job.
    pub priority: JobPriority,
}

/// A worker found nothing to do and is going to sleep.
///
/// It may be woken up again at once if work arrives in the same instant; the event is always followed by a
/// [`WorkerWake`].
#[derive(Copy, Clone, Debug)]
#[non_exhaustive]
pub struct WorkerSleep {
    /// The pool.
    pub pool: PoolId,
    /// The worker.
    pub worker: usize,
    /// When it happened.
    pub at: Instant,
}

/// A worker is awake again.
#[derive(Copy, Clone, Debug)]
#[non_exhaustive]
pub struct WorkerWake {
    /// The pool.
    pub pool: PoolId,
    /// The worker.
    pub worker: usize,
    /// How long the [`WorkerSleep`] before it lasted.
    pub slept: Duration,
}

/// Receives what happens in a pool. Every method has an empty default, so an observer implements only the events
/// it cares about.
///
/// ```
/// use std::sync::Arc;
/// use std::sync::atomic::{AtomicU64, Ordering};
/// use threading::{JobFinish, JobObserver, PoolConfig, ThreadPool, WorkerCount};
///
/// #[derive(Default)]
/// struct CountJobs(AtomicU64);
///
/// impl JobObserver for CountJobs {
///     fn job_finished(&self, _event: &JobFinish) {
///         self.0.fetch_add(1, Ordering::Relaxed);
///     }
/// }
///
/// let counter = Arc::new(CountJobs::default());
/// let pool = ThreadPool::new(
///     PoolConfig::new()
///         .with_workers(WorkerCount::Exact(1))
///         .with_observer(counter.clone()),
/// )?;
/// pool.spawn(|| ()).wait();
/// drop(pool); // the workers have finished when the pool is dropped
/// assert_eq!(counter.0.load(Ordering::Relaxed), 1);
/// # Ok::<(), threading::ThreadingError>(())
/// ```
///
/// # What a hook may do
///
/// The hooks run **on the thread the event happens on** (a worker, or a thread that helps), in the middle of the
/// scheduling code, so they must be fast and must not block: record the event in a buffer of your own and move on.
/// Do not spawn jobs on the pool being observed or wait for it from a hook. A hook that panics does not hurt the
/// pool: the panic is caught and dropped, and the event is lost.
///
/// Events of one thread arrive in order (`job_started`, then `job_finished` of the same job, with the events of
/// nested jobs in between); events of different threads are not ordered against each other, so sort by the
/// timestamps if a global order matters.
pub trait JobObserver: Send + Sync + 'static {
    /// A job is about to run.
    fn job_started(&self, _event: &JobStart) {}

    /// A job has finished running.
    fn job_finished(&self, _event: &JobFinish) {}

    /// A job was taken from another worker's queue. Called before the `job_started` of that job.
    fn job_stolen(&self, _event: &JobSteal) {}

    /// A worker is about to sleep.
    fn worker_sleeping(&self, _event: &WorkerSleep) {}

    /// A worker is awake again.
    fn worker_woke(&self, _event: &WorkerWake) {}
}

/// What one worker (or the helping threads, together) did, from the start of the pool until now.
///
/// All values only grow. Read twice and subtract to measure an interval (a frame, say).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct WorkerStats {
    /// Jobs run.
    pub jobs_run: u64,
    /// Jobs this thread took from the queue of *another worker*.
    pub steals: u64,
    /// Jobs other threads took from this worker's queue.
    pub stolen_from: u64,
    /// Sleeps that have ended: times the worker really slept (it was not kept awake by work that arrived in
    /// the same instant) and was woken. A sleep that is still going on is not counted yet.
    pub sleeps: u64,
    /// Time spent running jobs. Nested jobs (help while waiting) count once, with the job that ran them. Zero
    /// unless timing is on ([`PoolConfig::with_timing`](crate::PoolConfig::with_timing), or an observer).
    pub busy: Duration,
    /// Time spent asleep, added when each sleep ends. Zero unless timing is on.
    pub slept: Duration,
    /// How many times the scratch memory of this worker was reset for a new frame (see
    /// [`ThreadPool::begin_scratch_frame`](crate::ThreadPool::begin_scratch_frame)).
    pub scratch_frames: u64,
    /// The most scratch memory in bytes the worker had used in any frame that has ended.
    pub scratch_peak: usize,
}

impl WorkerStats {
    /// Adds `other` into `self` (`scratch_peak` takes the larger one).
    fn accumulate(&mut self, other: &Self) {
        self.jobs_run += other.jobs_run;
        self.steals += other.steals;
        self.stolen_from += other.stolen_from;
        self.sleeps += other.sleeps;
        self.busy += other.busy;
        self.slept += other.slept;
        self.scratch_frames += other.scratch_frames;
        self.scratch_peak = self.scratch_peak.max(other.scratch_peak);
    }
}

/// A snapshot of the statistics of a pool, from [`ThreadPool::stats`](crate::ThreadPool::stats).
///
/// The counters are read one by one without stopping the pool, so a snapshot taken while jobs run is not a
/// consistent cut (a job may be counted as run and not yet as stolen); it is exact once the pool is idle.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct PoolStats {
    /// One entry per worker, by worker index.
    pub workers: Vec<WorkerStats>,
    /// Everything the threads that are not workers of the pool did for it, together (a waiting thread that ran
    /// a job, the thread that shut the pool down, every job of a pool without workers).
    pub helpers: WorkerStats,
    /// The scratch memory each worker has in bytes (0 when the pool has none).
    pub scratch_capacity: usize,
}

impl PoolStats {
    /// The sum over all workers and the helpers.
    #[must_use]
    pub fn total(&self) -> WorkerStats {
        let mut total = self.helpers;
        for worker in &self.workers {
            total.accumulate(worker);
        }
        total
    }
}

/// The live counters of one worker (or of the helpers). Only that worker writes most of them, so they sit on a
/// cache line of their own.
#[derive(Default)]
#[repr(align(64))]
pub(crate) struct Counters {
    pub(crate) jobs: AtomicU64,
    pub(crate) steals: AtomicU64,
    pub(crate) stolen_from: AtomicU64,
    pub(crate) sleeps: AtomicU64,
    pub(crate) busy_nanos: AtomicU64,
    pub(crate) slept_nanos: AtomicU64,
    pub(crate) scratch_frames: AtomicU64,
    pub(crate) scratch_peak: AtomicU64,
}

impl Counters {
    pub(crate) fn snapshot(&self) -> WorkerStats {
        WorkerStats {
            jobs_run: self.jobs.load(Ordering::Relaxed),
            steals: self.steals.load(Ordering::Relaxed),
            stolen_from: self.stolen_from.load(Ordering::Relaxed),
            sleeps: self.sleeps.load(Ordering::Relaxed),
            busy: Duration::from_nanos(self.busy_nanos.load(Ordering::Relaxed)),
            slept: Duration::from_nanos(self.slept_nanos.load(Ordering::Relaxed)),
            // Read before the peak, see `Shared::refresh_scratch`: seeing a frame means seeing its peak.
            scratch_frames: self.scratch_frames.load(Ordering::Acquire),
            scratch_peak: usize::try_from(self.scratch_peak.load(Ordering::Relaxed))
                .unwrap_or(usize::MAX),
        }
    }
}

/// A duration in nanoseconds, saturating (u64 nanoseconds last 584 years).
pub(crate) fn nanos(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_total_adds_workers_and_helpers_and_keeps_the_largest_peak() {
        let stats = PoolStats {
            workers: vec![
                WorkerStats {
                    jobs_run: 3,
                    steals: 1,
                    scratch_peak: 100,
                    busy: Duration::from_millis(2),
                    ..WorkerStats::default()
                },
                WorkerStats {
                    jobs_run: 4,
                    stolen_from: 1,
                    scratch_peak: 300,
                    busy: Duration::from_millis(3),
                    ..WorkerStats::default()
                },
            ],
            helpers: WorkerStats {
                jobs_run: 2,
                scratch_peak: 50,
                ..WorkerStats::default()
            },
            scratch_capacity: 1024,
        };
        let total = stats.total();
        assert_eq!(total.jobs_run, 9);
        assert_eq!(total.steals, 1);
        assert_eq!(total.stolen_from, 1);
        assert_eq!(total.busy, Duration::from_millis(5));
        assert_eq!(total.scratch_peak, 300);
    }

    #[test]
    fn counters_snapshot_what_was_added() {
        let counters = Counters::default();
        counters.jobs.fetch_add(5, Ordering::Relaxed);
        counters.busy_nanos.fetch_add(1_500, Ordering::Relaxed);
        counters.scratch_peak.fetch_max(64, Ordering::Relaxed);
        let snapshot = counters.snapshot();
        assert_eq!(snapshot.jobs_run, 5);
        assert_eq!(snapshot.busy, Duration::from_nanos(1_500));
        assert_eq!(snapshot.scratch_peak, 64);
    }

    #[test]
    fn counters_sit_on_their_own_cache_line() {
        assert!(std::mem::align_of::<Counters>() >= 64);
        assert_eq!(std::mem::size_of::<Counters>() % 64, 0);
    }

    #[test]
    fn a_pool_id_prints_and_exposes_its_number() {
        let id = PoolId(7);
        assert_eq!(id.get(), 7);
        assert_eq!(id.to_string(), "pool#7");
    }

    #[test]
    fn nanos_saturates() {
        assert_eq!(nanos(Duration::from_nanos(9)), 9);
        assert_eq!(nanos(Duration::MAX), u64::MAX);
    }
}
