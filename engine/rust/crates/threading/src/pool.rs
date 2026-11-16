//! The thread pool: the workers, the shared state they run on, and the handles that spawn jobs.

use std::fmt;
use std::panic::{self, AssertUnwindSafe};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::{self, JoinHandle};

use platform::thread::ThreadPriority;

use crate::config::PoolConfig;
use crate::error::{ThreadingError, ThreadingResult};
use crate::job::{JobHandle, Slot};
use crate::priority::JobPriority;
use crate::queue::{Queues, Task};
use crate::sleep::Sleep;
use crate::worker;

/// Hands out a different id to every pool, so a thread can tell "a worker of this pool" from "a worker of
/// another pool".
static NEXT_POOL_ID: AtomicUsize = AtomicUsize::new(0);

/// What the workers, the pool, the spawners and the job handles share.
pub(crate) struct Shared {
    id: usize,
    queues: Queues,
    sleep: Sleep,
    /// Set once, when the pool is dropped: the workers finish the queues and leave.
    shutting_down: AtomicBool,
}

impl Shared {
    fn new(workers: usize) -> Self {
        Self {
            id: NEXT_POOL_ID.fetch_add(1, Ordering::Relaxed),
            queues: Queues::new(workers),
            sleep: Sleep::new(),
            shutting_down: AtomicBool::new(false),
        }
    }

    /// Queues a job and returns the handle for its result.
    ///
    /// A job spawned by one of this pool's own workers goes into that worker's queue; every other job goes
    /// into the shared queue, which refuses it once the pool has shut down.
    fn spawn_job<F, T>(
        self: &Arc<Self>,
        priority: JobPriority,
        job: F,
    ) -> ThreadingResult<JobHandle<T>>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        let slot = Arc::new(Slot::new());
        let completion = Arc::clone(&slot);
        let task: Task = Box::new(move || {
            let result = panic::catch_unwind(AssertUnwindSafe(job));
            completion.complete(result);
        });

        match worker::index_in(self.id) {
            Some(index) => self.queues.push_local(index, priority, task),
            None => {
                if self.queues.push_external(priority, task).is_err() {
                    return Err(ThreadingError::PoolClosed);
                }
            }
        }
        self.sleep.wake_one();
        Ok(JobHandle::new(slot, Arc::clone(self)))
    }

    /// Runs one queued job on the calling thread. `false` when nothing was queued.
    pub(crate) fn run_one(&self) -> bool {
        match self.queues.pop(worker::index_in(self.id)) {
            Some(task) => {
                task();
                true
            }
            None => false,
        }
    }
}

/// A pool of worker threads that run jobs.
///
/// Every worker has a queue of its own, and there is one shared queue for jobs that are spawned from outside
/// the pool. A worker runs the most urgent job it can find ([`JobPriority`]); an idle worker steals from the
/// others, so work spreads by itself. Jobs are closures; the result (or the panic) of a job comes back through
/// its [`JobHandle`].
///
/// # Order
///
/// * A job of a higher [`JobPriority`] always starts before a queued job of a lower one.
/// * Within a priority, jobs spawned from outside the pool start in the order they were spawned.
/// * A job spawned by a job on a worker goes into that worker's own queue. The worker runs its newest job
///   first, other workers steal the oldest. Nothing is promised about the order of such jobs beyond that, and
///   with more than one worker nothing is promised about the order in which jobs *finish*.
///
/// # Ownership
///
/// The pool is an ordinary value, not a global: the code that creates it owns it, passes `&ThreadPool` (or an
/// `Arc<ThreadPool>`) to whoever needs to spawn, and drops it to stop the workers. A job that wants to spawn
/// more jobs should carry a [`Spawner`] ([`spawner`](Self::spawner)), which can spawn but does not keep the
/// pool alive. Dropping the pool (or [`shutdown`](Self::shutdown)) first finishes every job that is still
/// queued, including the jobs those jobs spawn, then joins the workers.
///
/// ```
/// use threading::{PoolConfig, ThreadPool, WorkerCount};
///
/// let pool = ThreadPool::new(PoolConfig::new().with_workers(WorkerCount::Exact(2)))?;
/// let handles: Vec<_> = (0..4_u64).map(|n| pool.spawn(move || n * n)).collect();
/// let squares: Vec<u64> = handles.into_iter().map(|handle| handle.wait()).collect();
/// assert_eq!(squares, [0, 1, 4, 9]);
/// # Ok::<(), threading::ThreadingError>(())
/// ```
pub struct ThreadPool {
    shared: Arc<Shared>,
    workers: Vec<JoinHandle<()>>,
    worker_count: usize,
    name_prefix: String,
}

impl ThreadPool {
    /// Starts a pool.
    ///
    /// # Errors
    ///
    /// * [`ThreadingError::InvalidConfig`] when [`PoolConfig::validate`] rejects the config.
    /// * [`ThreadingError::Spawn`] when the OS refuses to start a worker. The workers that were already
    ///   running are stopped first.
    pub fn new(config: PoolConfig) -> ThreadingResult<Self> {
        config.validate()?;
        let worker_count = config.workers().resolve();
        let shared = Arc::new(Shared::new(worker_count));
        let mut workers = Vec::with_capacity(worker_count);

        for index in 0..worker_count {
            let name = format!("{}-{index}", config.name_prefix());
            let worker_shared = Arc::clone(&shared);
            let priority = config.thread_priority();
            let started = thread::Builder::new()
                .name(name.clone())
                .spawn(move || worker_main(&worker_shared, index, priority));
            match started {
                Ok(handle) => workers.push(handle),
                Err(source) => {
                    stop(&shared, workers);
                    return Err(ThreadingError::Spawn {
                        index,
                        name,
                        source,
                    });
                }
            }
        }

        Ok(Self {
            shared,
            workers,
            worker_count,
            name_prefix: config.name_prefix().to_owned(),
        })
    }

    /// The number of worker threads (0 for a pool in single-threaded mode).
    #[must_use]
    pub fn worker_count(&self) -> usize {
        self.worker_count
    }

    /// The number of jobs that are queued and not yet running.
    #[must_use]
    pub fn pending_jobs(&self) -> usize {
        self.shared.queues.queued()
    }

    /// Queues a job with [`JobPriority::Normal`] and returns the handle for its result.
    ///
    /// The job runs on a worker, or on a thread that waits for a job (see [`JobHandle`]). A job that panics
    /// does not hurt the pool: the panic is stored and re-raised (or returned) by the handle's wait.
    pub fn spawn<F, T>(&self, job: F) -> JobHandle<T>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        self.spawn_with_priority(JobPriority::Normal, job)
    }

    /// Queues a job with the given priority and returns the handle for its result.
    ///
    /// See [`spawn`](Self::spawn) for how the job runs and [`JobPriority`] for what the priority means.
    pub fn spawn_with_priority<F, T>(&self, priority: JobPriority, job: F) -> JobHandle<T>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        match self.shared.spawn_job(priority, job) {
            Ok(handle) => handle,
            Err(_) => unreachable!(
                "a pool only closes while it is being dropped, and nobody can spawn then"
            ),
        }
    }

    /// A [`Spawner`] for this pool: a cheap, cloneable handle that can spawn jobs from anywhere (including
    /// from inside jobs) without keeping the pool alive.
    #[must_use]
    pub fn spawner(&self) -> Spawner {
        Spawner {
            shared: Arc::clone(&self.shared),
        }
    }

    /// Runs queued jobs on the calling thread, one after the other, until nothing is queued, and returns how
    /// many it ran.
    ///
    /// This is how fire-and-forget jobs get done in single-threaded mode (no worker exists to run them and
    /// nobody waits for them). With workers it also works, the calling thread then simply helps.
    pub fn run_pending(&self) -> usize {
        let mut ran = 0;
        while self.shared.run_one() {
            ran += 1;
        }
        ran
    }

    /// Finishes every queued job, stops the workers and waits for them. Dropping the pool does the same;
    /// this only makes the point in time explicit.
    pub fn shutdown(self) {
        drop(self);
    }
}

impl Drop for ThreadPool {
    fn drop(&mut self) {
        stop(&self.shared, std::mem::take(&mut self.workers));
    }
}

impl fmt::Debug for ThreadPool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ThreadPool")
            .field("name_prefix", &self.name_prefix)
            .field("worker_count", &self.worker_count)
            .field("pending_jobs", &self.pending_jobs())
            .finish()
    }
}

/// A handle that spawns jobs into a [`ThreadPool`] without owning it.
///
/// This is what a job carries when it wants to start more jobs: it is `Clone`, `Send`, `Sync` and `'static`, so
/// it can be moved into any closure, and unlike an `Arc<ThreadPool>` it does not make the job the possible last
/// owner of the pool. Get one with [`ThreadPool::spawner`].
///
/// A job spawned by a job running on a worker of the pool goes into that worker's own queue (cheap, and other
/// workers steal it when they are idle); a job spawned from any other thread goes into the shared queue.
///
/// Spawning fails with [`ThreadingError::PoolClosed`] once the pool has finished shutting down. While it is
/// still shutting down (it drains the queued jobs first) jobs that the running jobs spawn are accepted and run.
///
/// ```
/// use threading::{PoolConfig, ThreadPool, WorkerCount};
///
/// let pool = ThreadPool::new(PoolConfig::new().with_workers(WorkerCount::Exact(2)))?;
/// let spawner = pool.spawner();
/// let outer = pool.spawn(move || {
///     // A job starts another job and waits for it.
///     let inner = spawner.spawn(|| 20).expect("the pool is running");
///     inner.wait() + 1
/// });
/// assert_eq!(outer.wait(), 21);
/// # Ok::<(), threading::ThreadingError>(())
/// ```
#[derive(Clone)]
pub struct Spawner {
    shared: Arc<Shared>,
}

impl Spawner {
    /// Queues a job with [`JobPriority::Normal`].
    ///
    /// # Errors
    ///
    /// [`ThreadingError::PoolClosed`] when the pool has finished shutting down; the job is dropped unrun.
    pub fn spawn<F, T>(&self, job: F) -> ThreadingResult<JobHandle<T>>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        self.spawn_with_priority(JobPriority::Normal, job)
    }

    /// Queues a job with the given priority.
    ///
    /// # Errors
    ///
    /// [`ThreadingError::PoolClosed`] when the pool has finished shutting down; the job is dropped unrun.
    pub fn spawn_with_priority<F, T>(
        &self,
        priority: JobPriority,
        job: F,
    ) -> ThreadingResult<JobHandle<T>>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        self.shared.spawn_job(priority, job)
    }
}

impl fmt::Debug for Spawner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Spawner")
            .field("pending_jobs", &self.shared.queues.queued())
            .finish()
    }
}

/// The body of a worker thread.
fn worker_main(shared: &Shared, index: usize, priority: ThreadPriority) {
    if priority != ThreadPriority::Normal {
        // A request: when the OS refuses (Linux without the privilege to raise a priority), the worker keeps
        // the default priority and does its work all the same.
        let _ = platform::thread::set_priority(priority);
    }
    worker::enter(shared.id, index);

    // Jobs catch their own panics (see `Shared::spawn_job`), so this loop never unwinds.
    loop {
        if let Some(task) = shared.queues.pop(Some(index)) {
            task();
            continue;
        }
        if shared.queues.queued() > 0 {
            // A job is being pushed right now (its counter is raised a moment before it becomes visible).
            thread::yield_now();
            continue;
        }
        if shared.shutting_down.load(Ordering::SeqCst) {
            return;
        }
        shared.sleep.wait_unless(|| {
            shared.queues.queued() > 0 || shared.shutting_down.load(Ordering::SeqCst)
        });
    }
}

/// Stops the workers: lets them finish the queues, joins them, finishes what is left on this thread and closes
/// the shared queue for good.
fn stop(shared: &Shared, workers: Vec<JoinHandle<()>>) {
    shared.shutting_down.store(true, Ordering::SeqCst);
    shared.sleep.wake_all();

    let current = thread::current().id();
    for worker in workers {
        // The last owner of a pool can be a job on one of its own workers (the job held an `Arc<ThreadPool>`).
        // A thread cannot join itself; that worker leaves by itself as soon as the job returns, because the
        // shutdown flag is set and the queues are drained below.
        if worker.thread().id() != current {
            // A worker never panics (jobs catch their panics), so there is nothing to report.
            let _ = worker.join();
        }
    }

    // With no workers (single-threaded mode) nobody has run the queued jobs; running them here keeps the
    // promise that a pool finishes its jobs, and no handle waits forever. The shared queue closes only when it
    // is seen empty under its lock, so a `Spawner` on another thread either gets its job in before that (and
    // the loop runs it) or is told the pool is closed.
    loop {
        while shared.run_one() {}
        if shared.queues.close_if_empty() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::WorkerCount;

    fn inline_pool() -> ThreadPool {
        ThreadPool::new(PoolConfig::new().with_workers(WorkerCount::Exact(0))).unwrap()
    }

    #[test]
    fn pool_ids_differ() {
        let a = inline_pool();
        let b = inline_pool();
        assert_ne!(a.shared.id, b.shared.id);
    }

    #[test]
    fn a_closed_pool_refuses_outside_spawns_and_names_the_error() {
        let pool = inline_pool();
        let spawner = pool.spawner();
        drop(pool);
        let err = spawner.spawn(|| ()).unwrap_err();
        assert!(matches!(err, ThreadingError::PoolClosed), "{err}");
    }

    #[test]
    fn a_spawner_is_cheap_to_clone_and_prints() {
        let pool = inline_pool();
        let spawner = pool.spawner();
        let copy = spawner.clone();
        drop(copy.spawn(|| ()).unwrap());
        assert!(format!("{spawner:?}").contains("pending_jobs: 1"));
    }
}
