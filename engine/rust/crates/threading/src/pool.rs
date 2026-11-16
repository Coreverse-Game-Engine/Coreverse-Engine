//! The thread pool: a shared queue and the workers that drain it.

use std::collections::VecDeque;
use std::fmt;
use std::panic::{self, AssertUnwindSafe};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::{self, JoinHandle};

use platform::thread::ThreadPriority;

use crate::config::PoolConfig;
use crate::error::{ThreadingError, ThreadingResult};
use crate::job::{JobHandle, Slot};
use crate::sync::lock;

/// One queued unit of work. Everything a job needs (its closure, the slot for its result) is inside.
type Task = Box<dyn FnOnce() + Send + 'static>;

/// What the workers, the pool and the job handles share.
pub(crate) struct Shared {
    queue: Mutex<Queue>,
    work_available: Condvar,
}

struct Queue {
    tasks: VecDeque<Task>,
    /// Set once; the workers leave when the queue is empty and this is set.
    shutdown: bool,
}

impl Shared {
    fn new() -> Self {
        Self {
            queue: Mutex::new(Queue {
                tasks: VecDeque::new(),
                shutdown: false,
            }),
            work_available: Condvar::new(),
        }
    }

    fn push(&self, task: Task) {
        lock(&self.queue).tasks.push_back(task);
        self.work_available.notify_one();
    }

    fn pop(&self) -> Option<Task> {
        lock(&self.queue).tasks.pop_front()
    }

    fn len(&self) -> usize {
        lock(&self.queue).tasks.len()
    }

    /// Runs one queued job on the calling thread. `false` when the queue was empty.
    pub(crate) fn run_one(&self) -> bool {
        match self.pop() {
            Some(task) => {
                task();
                true
            }
            None => false,
        }
    }

    /// Blocks until there is a job or the pool is shutting down; `None` means "shut down and nothing left".
    fn next_for_worker(&self) -> Option<Task> {
        let mut queue = lock(&self.queue);
        loop {
            if let Some(task) = queue.tasks.pop_front() {
                return Some(task);
            }
            if queue.shutdown {
                return None;
            }
            queue = self
                .work_available
                .wait(queue)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }
}

/// A pool of worker threads that run jobs.
///
/// Jobs go into one first-in-first-out queue ([`spawn`](Self::spawn)); idle workers take them in turn. A job is
/// any closure; its result (or its panic) comes back through the [`JobHandle`].
///
/// The pool is an ordinary value, not a global: the code that creates it owns it, passes `&ThreadPool` (or an
/// `Arc<ThreadPool>`) to whoever needs to spawn, and drops it to stop the workers. Dropping (or
/// [`shutdown`](Self::shutdown)) first finishes every job that is still queued, then joins the workers.
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
        let shared = Arc::new(Shared::new());
        let mut workers = Vec::with_capacity(worker_count);

        for index in 0..worker_count {
            let name = format!("{}-{index}", config.name_prefix());
            let worker_shared = Arc::clone(&shared);
            let priority = config.thread_priority();
            let started = thread::Builder::new()
                .name(name.clone())
                .spawn(move || worker_main(&worker_shared, priority));
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
        self.shared.len()
    }

    /// Queues a job and returns the handle for its result.
    ///
    /// The job runs on a worker, or on the thread that waits for it (see [`JobHandle`]). Jobs that were queued
    /// first start first; with more than one worker they may of course finish in any order.
    ///
    /// A job that panics does not hurt the pool: the panic is stored and re-raised (or returned) by the
    /// handle's wait.
    pub fn spawn<F, T>(&self, job: F) -> JobHandle<T>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        let slot = Arc::new(Slot::new());
        let completion = Arc::clone(&slot);
        self.shared.push(Box::new(move || {
            let result = panic::catch_unwind(AssertUnwindSafe(job));
            completion.complete(result);
        }));
        JobHandle::new(slot, Arc::clone(&self.shared))
    }

    /// Runs every queued job on the calling thread, one after the other, and returns how many it ran.
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

/// The body of a worker thread.
fn worker_main(shared: &Shared, priority: ThreadPriority) {
    if priority != ThreadPriority::Normal {
        // A request: when the OS refuses (Linux without the privilege to raise a priority), the worker keeps
        // the default priority and does its work all the same.
        let _ = platform::thread::set_priority(priority);
    }
    // Jobs catch their own panics (see `ThreadPool::spawn`), so this loop never unwinds.
    while let Some(task) = shared.next_for_worker() {
        task();
    }
}

/// Stops the workers: lets them finish the queue, joins them, and finishes what is left on this thread.
fn stop(shared: &Shared, workers: Vec<JoinHandle<()>>) {
    lock(&shared.queue).shutdown = true;
    shared.work_available.notify_all();

    let current = thread::current().id();
    for worker in workers {
        // The last owner of a pool can be a job on one of its own workers (the job held an `Arc<ThreadPool>`).
        // A thread cannot join itself; that worker leaves by itself as soon as the job returns, because the
        // shutdown flag is set and the queue gets drained below.
        if worker.thread().id() != current {
            // A worker never panics (jobs catch their panics), so there is nothing to report.
            let _ = worker.join();
        }
    }

    // With no workers (single-threaded mode) nobody has run the queued jobs; running them here keeps the
    // promise that a pool finishes its jobs, and no handle waits forever.
    while shared.run_one() {}
}
