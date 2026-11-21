//! The thread pool: the workers, the shared state they run on, and the handles that spawn jobs.

use std::fmt;
use std::panic::{self, AssertUnwindSafe};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Instant;

use memory::FrameArena;

use platform::thread::ThreadPriority;

use crate::config::PoolConfig;
use crate::error::{ThreadingError, ThreadingResult};
use crate::job::{JobHandle, Slot, discard_payload, join_all};
use crate::observe::{
    Counters, JobFinish, JobObserver, JobStart, JobSteal, PoolId, PoolStats, WorkerSleep,
    WorkerWake, nanos,
};
use crate::priority::JobPriority;
use crate::queue::{Origin, Queues, Task};
use crate::role::{self, ThreadRole};
use crate::scratch;
use crate::sleep::Sleep;
use crate::worker;

/// Hands out a different id to every pool, so a thread can tell "a worker of this pool" from "a worker of
/// another pool".
static NEXT_POOL_ID: AtomicUsize = AtomicUsize::new(0);

/// What the workers, the pool, the spawners and the job handles share.
pub(crate) struct Shared {
    id: usize,
    worker_count: usize,
    queues: Queues,
    sleep: Sleep,
    /// Set once, when the pool is dropped: the workers finish the queues and leave.
    shutting_down: AtomicBool,
    /// Receives the events of the pool, if the config installed one.
    observer: Option<Arc<dyn JobObserver>>,
    /// Whether to read the clock around jobs and sleeps: on when an observer is installed or the config asks.
    timing: bool,
    /// Numbers the jobs for the observer, in the order they start.
    next_job: AtomicU64,
    /// What each worker did, by worker index.
    counters: Vec<Counters>,
    /// What all the threads that are not workers of this pool did for it.
    helpers: Counters,
    /// The size of each worker's scratch arena in bytes; 0 when there is none.
    scratch_capacity: usize,
    /// Raised by [`ThreadPool::begin_scratch_frame`]; every worker resets its arena when it sees a new value.
    scratch_epoch: AtomicU64,
}

impl Shared {
    fn new(workers: usize, config: &PoolConfig) -> Self {
        let observer = config.observer().cloned();
        Self {
            id: NEXT_POOL_ID.fetch_add(1, Ordering::Relaxed),
            worker_count: workers,
            queues: Queues::new(workers),
            sleep: Sleep::new(),
            shutting_down: AtomicBool::new(false),
            timing: config.timing() || observer.is_some(),
            observer,
            next_job: AtomicU64::new(0),
            counters: (0..workers).map(|_| Counters::default()).collect(),
            helpers: Counters::default(),
            scratch_capacity: config.scratch_capacity().unwrap_or(0),
            scratch_epoch: AtomicU64::new(0),
        }
    }

    fn pool_id(&self) -> PoolId {
        PoolId(self.id)
    }

    /// The counters of worker `index`, or the helpers' for a thread that is not a worker of this pool.
    fn counters_of(&self, worker: Option<usize>) -> &Counters {
        worker
            .and_then(|index| self.counters.get(index))
            .unwrap_or(&self.helpers)
    }

    /// Calls a hook of the observer, if there is one. A hook that panics must not take a worker down: the
    /// panic is caught and dropped.
    fn notify(&self, hook: impl FnOnce(&dyn JobObserver)) {
        if let Some(observer) = &self.observer
            && let Err(payload) = panic::catch_unwind(AssertUnwindSafe(|| hook(observer.as_ref())))
        {
            discard_payload(payload);
        }
    }

    fn stats(&self) -> PoolStats {
        PoolStats {
            workers: self.counters.iter().map(Counters::snapshot).collect(),
            helpers: self.helpers.snapshot(),
            scratch_capacity: self.scratch_capacity,
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

        if self.submit(priority, task).is_err() {
            return Err(ThreadingError::PoolClosed);
        }
        Ok(JobHandle::new(slot, Arc::clone(self)))
    }

    /// Queues a task and wakes a sleeping worker.
    ///
    /// * A task from one of this pool's own workers goes into that worker's queue. This is never refused: the
    ///   worker is still running and will find it.
    /// * Any other task goes into the shared queue, which gives the task back (`Err`) in two cases: the pool has
    ///   finished shutting down, or the shutdown has begun and the calling thread is not running one of this
    ///   pool's jobs.
    ///
    /// The second case is what keeps a shutdown finite. The shutdown has to run everything that is queued, and
    /// the jobs that running jobs spawn (those are accepted, hence the exception); but if any thread holding a
    /// spawner could add work for as long as it likes, the queue might never be seen empty, the workers would
    /// never leave and the pool would never finish shutting down.
    pub(crate) fn submit(&self, priority: JobPriority, task: Task) -> Result<(), Task> {
        let result = match worker::index_in(self.id) {
            Some(index) => {
                self.queues.push_local(index, priority, task);
                Ok(())
            }
            None => {
                if self.shutting_down.load(Ordering::SeqCst) && !worker::is_running_job_of(self.id)
                {
                    return Err(task);
                }
                self.queues.push_external(priority, task)
            }
        };
        if result.is_ok() {
            self.sleep.wake_one();
        }
        result
    }

    /// Like [`submit`](Self::submit), but a task the pool refuses runs on the calling thread instead of being
    /// dropped. For work that must happen however late the call comes (continuations of finished jobs, graph
    /// tasks), the same fallback a scoped job uses.
    pub(crate) fn submit_or_run(&self, priority: JobPriority, task: Task) {
        if let Err(task) = self.submit(priority, task) {
            self.run_task(task, priority, Origin::Inline);
        }
    }

    /// The number of worker threads.
    pub(crate) fn worker_count(&self) -> usize {
        self.worker_count
    }

    /// Whether any job is counted as queued. `true` can also mean "being pushed right now".
    pub(crate) fn has_queued(&self) -> bool {
        self.queues.queued() > 0
    }

    /// Runs one task on the calling thread, which counts as running one of this pool's jobs meanwhile (see
    /// [`submit`](Self::submit)), and keeps the statistics and the observer informed.
    fn run_task(&self, task: Task, priority: JobPriority, origin: Origin) {
        let worker = worker::index_in(self.id);
        let counters = self.counters_of(worker);
        counters.jobs.fetch_add(1, Ordering::Relaxed);
        if let Origin::Stolen { victim } = origin {
            counters.steals.fetch_add(1, Ordering::Relaxed);
            if let Some(victim_counters) = self.counters.get(victim) {
                victim_counters.stolen_from.fetch_add(1, Ordering::Relaxed);
            }
            self.notify(|observer| {
                observer.job_stolen(&JobSteal {
                    pool: self.pool_id(),
                    thief: worker,
                    victim,
                    priority,
                });
            });
        }

        if !self.timing {
            let _running = worker::enter_job(self.id);
            task();
            return;
        }

        let job = self.next_job.fetch_add(1, Ordering::Relaxed);
        // A job that runs inside another job (help while waiting) is inside the other one's span; only the
        // outermost job adds to the busy time.
        let outermost = worker::running_depth() == 0;
        let started = Instant::now();
        self.notify(|observer| {
            observer.job_started(&JobStart {
                pool: self.pool_id(),
                job,
                worker,
                priority,
                at: started,
            });
        });
        {
            let _running = worker::enter_job(self.id);
            task();
        }
        let duration = started.elapsed();
        if outermost {
            counters
                .busy_nanos
                .fetch_add(nanos(duration), Ordering::Relaxed);
        }
        self.notify(|observer| {
            observer.job_finished(&JobFinish {
                pool: self.pool_id(),
                job,
                worker,
                priority,
                started,
                duration,
            });
        });
    }

    /// Runs one queued job on the calling thread. `false` when nothing was queued.
    pub(crate) fn run_one(&self) -> bool {
        match self.queues.pop(worker::index_in(self.id)) {
            Some(popped) => {
                self.run_task(popped.task, popped.priority, popped.origin);
                true
            }
            None => false,
        }
    }

    /// Resets the scratch arena of the calling worker if a new frame has been announced since it last looked.
    fn refresh_scratch(&self, index: usize, seen_epoch: &mut u64) {
        let epoch = self.scratch_epoch.load(Ordering::Acquire);
        if epoch == *seen_epoch {
            return;
        }
        *seen_epoch = epoch;
        if let Some(used) = scratch::begin_frame() {
            let counters = &self.counters[index];
            // The peak first, the frame count second (with `Release`): a snapshot that sees the new frame count
            // (it reads it with `Acquire`) also sees the peak of the frame that ended.
            counters
                .scratch_peak
                .fetch_max(u64::try_from(used).unwrap_or(u64::MAX), Ordering::Relaxed);
            counters.scratch_frames.fetch_add(1, Ordering::Release);
        }
    }

    /// A worker that found nothing to do sleeps until there is something again: a queued job, the shutdown, or a
    /// new scratch frame that it has not seen yet (`seen_epoch` is the frame it last reset for).
    ///
    /// The frame belongs in the check under the sleep lock. `begin_scratch_frame` raises the epoch and then takes
    /// that lock to notify; a worker that had looked at the epoch just before it changed and reaches the lock
    /// after the notification would otherwise find nothing to stay awake for and sleep through the frame, until
    /// some unrelated job woke it.
    fn idle(&self, index: usize, seen_epoch: u64) {
        let started = self.timing.then(Instant::now);
        if let Some(at) = started {
            self.notify(|observer| {
                observer.worker_sleeping(&WorkerSleep {
                    pool: self.pool_id(),
                    worker: index,
                    at,
                });
            });
        }
        let slept = self.sleep.wait_unless(|| {
            self.queues.queued() > 0
                || self.shutting_down.load(Ordering::SeqCst)
                || self.scratch_epoch.load(Ordering::Acquire) != seen_epoch
        });
        let counters = &self.counters[index];
        if slept {
            counters.sleeps.fetch_add(1, Ordering::Relaxed);
        }
        if let Some(at) = started {
            let duration = at.elapsed();
            if slept {
                counters
                    .slept_nanos
                    .fetch_add(nanos(duration), Ordering::Relaxed);
            }
            self.notify(|observer| {
                observer.worker_woke(&WorkerWake {
                    pool: self.pool_id(),
                    worker: index,
                    slept: duration,
                });
            });
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
/// queued, including the jobs those jobs spawn, then joins the workers. From the moment the shutdown begins, a
/// spawner refuses jobs from any thread that is not running one of the pool's jobs, so the shutdown ends after
/// a bounded amount of work, whatever other threads do.
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
    pub(crate) shared: Arc<Shared>,
    workers: Vec<JoinHandle<()>>,
    worker_count: usize,
    name_prefix: String,
    role: ThreadRole,
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

        // The scratch arenas are made here, so that a failure to allocate one is an error of `new`, before any
        // thread exists; each is then moved into its worker.
        let mut arenas: Vec<Option<FrameArena>> = Vec::with_capacity(worker_count);
        for _ in 0..worker_count {
            arenas.push(match config.scratch_capacity() {
                Some(capacity) => Some(
                    FrameArena::with_capacity_tagged(capacity, config.scratch_tag())
                        .map_err(ThreadingError::Scratch)?,
                ),
                None => None,
            });
        }

        let shared = Arc::new(Shared::new(worker_count, &config));
        let mut workers = Vec::with_capacity(worker_count);

        for (index, arena) in arenas.into_iter().enumerate() {
            let name = format!("{}-{index}", config.name_prefix());
            let worker_shared = Arc::clone(&shared);
            let priority = config.thread_priority();
            let role = config.role();
            let started = thread::Builder::new()
                .name(name.clone())
                .spawn(move || worker_main(&worker_shared, index, priority, role, arena));
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
            role: config.role(),
        })
    }

    /// The role the pool's workers have (see [`PoolConfig::with_role`]).
    #[must_use]
    pub fn role(&self) -> ThreadRole {
        self.role
    }

    /// The id of this pool, which the events of a [`JobObserver`] carry.
    #[must_use]
    pub fn id(&self) -> PoolId {
        self.shared.pool_id()
    }

    /// A snapshot of what the workers (and the threads that helped) did so far; see [`PoolStats`].
    #[must_use]
    pub fn stats(&self) -> PoolStats {
        self.shared.stats()
    }

    /// Announces a new frame to the scratch arenas of the workers (see [`PoolConfig::with_scratch`]): each
    /// worker frees its arena the next time it is between jobs, and not while a job still borrows it with
    /// [`with_scratch`](crate::with_scratch). It does nothing in a pool without scratch memory.
    ///
    /// Call it once per frame, from the thread that runs the frame loop, after the frame's jobs are done (or
    /// at the start of the next frame, before spawning). It does not wait and takes no lock: it raises a
    /// counter and wakes the sleeping workers so that they can reset at once. Calling it more often than
    /// workers get between jobs merges the announcements into one reset.
    pub fn begin_scratch_frame(&self) {
        if self.shared.scratch_capacity == 0 {
            return;
        }
        self.shared.scratch_epoch.fetch_add(1, Ordering::Release);
        self.shared.sleep.wake_all();
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

    /// Returns a handle for a job that is done when every job of `handles` is done, with the results in the
    /// order of the input. The handles are consumed.
    ///
    /// This is the counter-based dependency of the crate: nothing waits and nothing is queued; the job that
    /// finishes last completes the returned handle. If jobs panicked, the handle reports the panic of the
    /// first of them in input order (after all of them finished); the other panic payloads are dropped. An
    /// empty input is done at once with an empty `Vec`.
    ///
    /// Combine it with [`JobHandle::then`] to run something after a whole group:
    ///
    /// ```
    /// use threading::{PoolConfig, ThreadPool};
    ///
    /// let pool = ThreadPool::new(PoolConfig::new())?;
    /// let parts: Vec<_> = (1..=4_u64).map(|n| pool.spawn(move || n * 10)).collect();
    /// let total = pool.when_all(parts).then(|values| values.iter().sum::<u64>());
    /// assert_eq!(total.wait(), 100);
    /// # Ok::<(), threading::ThreadingError>(())
    /// ```
    pub fn when_all<T, I>(&self, handles: I) -> JobHandle<Vec<T>>
    where
        I: IntoIterator<Item = JobHandle<T>>,
        T: Send + 'static,
    {
        join_all(&self.shared, handles.into_iter().collect())
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
/// Spawning fails with [`ThreadingError::PoolClosed`] once the pool has begun to shut down, unless the calling
/// thread is running one of the pool's jobs: the jobs that running jobs spawn are part of the work the shutdown
/// finishes, so they are accepted and run, while a thread that merely holds a spawner cannot keep the pool from
/// ever finishing. A job that was accepted always runs.
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
    pub(crate) shared: Arc<Shared>,
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

    /// Like [`ThreadPool::when_all`]: a handle that is done when every job of `handles` is done.
    pub fn when_all<T, I>(&self, handles: I) -> JobHandle<Vec<T>>
    where
        I: IntoIterator<Item = JobHandle<T>>,
        T: Send + 'static,
    {
        join_all(&self.shared, handles.into_iter().collect())
    }

    /// The id of the pool (see [`ThreadPool::id`]).
    #[must_use]
    pub fn id(&self) -> PoolId {
        self.shared.pool_id()
    }

    /// A snapshot of the statistics of the pool (see [`ThreadPool::stats`]).
    #[must_use]
    pub fn stats(&self) -> PoolStats {
        self.shared.stats()
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
fn worker_main(
    shared: &Shared,
    index: usize,
    priority: ThreadPriority,
    role: ThreadRole,
    arena: Option<FrameArena>,
) {
    if priority != ThreadPriority::Normal {
        // A request: when the OS refuses (Linux without the privilege to raise a priority), the worker keeps
        // the default priority and does its work all the same.
        let _ = platform::thread::set_priority(priority);
    }
    worker::enter(shared.id, index);
    role::set_current(role);
    if let Some(arena) = arena {
        scratch::install(arena);
    }
    // Frames announced before this worker existed are not its business.
    let mut seen_epoch = shared.scratch_epoch.load(Ordering::Acquire);

    // Jobs catch their own panics (see `Shared::spawn_job`), so this loop never unwinds.
    loop {
        // Between jobs: nothing on this thread borrows the arena, so it may be reset.
        shared.refresh_scratch(index, &mut seen_epoch);
        if let Some(popped) = shared.queues.pop(Some(index)) {
            shared.run_task(popped.task, popped.priority, popped.origin);
            continue;
        }
        if shared.queues.queued() > 0 {
            // A job is being pushed right now (its counter is raised a moment before it becomes visible).
            thread::yield_now();
            continue;
        }
        if shared.shutting_down.load(Ordering::SeqCst) {
            scratch::uninstall();
            return;
        }
        shared.idle(index, seen_epoch);
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
