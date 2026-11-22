//! Scoped jobs: jobs that borrow from the stack of the code that spawns them.
//!
//! The pool's queues hold `'static` jobs, so a job that borrows a local variable cannot go in directly. This
//! module bridges the gap the way `std::thread::scope` does: [`run_scope`] does not return (and does not
//! unwind) until every job spawned in the scope has run to the end and has been dropped, so nothing a job
//! borrows can be gone while the job can still touch it. Only then is it sound to hide the borrow's lifetime
//! from the queues.
//!
//! # Safety design
//!
//! This is the only module of the crate with `unsafe` code, and it has exactly one `unsafe` block
//! ([`erase_lifetime`]). It is sound because of these invariants:
//!
//! 1. **Counting.** Every spawned job owns a [`ScopeGuard`]. Creating the guard raises `pending` in the scope's
//!    [`ScopeState`]; dropping it lowers `pending`. The guard lives inside the task, so it is dropped when the
//!    task has run *or* when the task is dropped without running (the pool never does that, but the argument
//!    does not depend on it).
//! 2. **Order of drops.** In [`ScopedTask`] the job is declared before the guard, and struct fields drop in
//!    declaration order, so the job (and everything it borrowed or captured) is gone before `pending` is
//!    lowered. A job that runs is consumed by its call, again before the guard goes.
//! 3. **No early exit.** [`run_scope`] runs the scope's body inside `catch_unwind`, then waits until `pending`
//!    is 0, and only then returns the body's value or re-raises a panic. The wait cannot panic; if it somehow
//!    did, an abort guard stops the process rather than let the stack frame go while jobs may still use it.
//!    While waiting, the thread runs queued jobs itself instead of sleeping, so a scope makes progress even
//!    with no worker threads.
//! 4. **Visibility.** `pending` lives under a mutex. A job's last write to borrowed data happens before its
//!    guard locks that mutex to lower the counter, and the waiter reads 0 under the same mutex before it
//!    returns, so the caller sees every write the jobs made.
//! 5. **Lifetimes.** `'scope` is invariant and higher-ranked in [`run_scope`]'s signature (the same shape as
//!    `std::thread::scope`), so a `&Scope` cannot be smuggled out of the closure, and a job may only borrow
//!    data that outlives the whole call (`'env`).
//! 6. **Panics.** A panicking job is caught inside its task (the worker thread never unwinds), its payload is
//!    stored in the scope, and the scope re-raises the first one after every job is done.
//! 7. **No protected borrow outlives the release.** Lowering `pending` wakes the scope's owner, which may then
//!    read data the job wrote through a `&mut`. Under Stacked Borrows (Miri) a reference that is part of a
//!    function argument stays protected until that function returns, and that includes references hidden in the
//!    fields of a by-value closure environment. The task closure therefore captures the [`ScopedTask`] behind a
//!    `Box` (whose pointee is not retagged as part of the argument) and moves the job out onto its own stack
//!    frame; the job's call returns, and its borrows end, strictly before the guard is dropped.
//! 8. **A task that can be taken back stays accounted for.** [`Scope::spawn_claimable`] puts the erased task
//!    (job and guard) into an `Arc<Mutex<Option<Task>>>` and queues a `'static` shell that takes the task out of
//!    the cell and runs it. Whoever takes the task out first runs it (the shell, or the owner through
//!    [`Claim::run_here`]); the other finds `None`. The guard travels with the task, so invariant 1 holds
//!    unchanged: `pending` stays raised while the task is in the cell, and is lowered when the task has run or
//!    has been dropped (the cell is dropped with its last `Arc`: the queued shell, or the claim). A shell that
//!    is popped after the task is gone holds no borrowed data, so it may outlive the scope. No new `unsafe`
//!    code: the lifetime is erased once, in [`erase_lifetime`], before the task goes into the cell.

use std::any::Any;
use std::fmt;
use std::marker::PhantomData;
use std::mem;
use std::panic::{self, AssertUnwindSafe};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread;
use std::time::Duration;

use crate::job::PanicPayload;
use crate::pool::Shared;
use crate::priority::JobPriority;
use crate::queue::Task;
use crate::sync::{IDLE_POLL, lock};

/// What a scope remembers about its jobs.
struct ScopeState {
    inner: Mutex<Inner>,
    idle: Condvar,
}

struct Inner {
    /// Jobs spawned in the scope that have not been dropped yet.
    pending: usize,
    /// The panic payload of the first job that panicked.
    panic: Option<PanicPayload>,
}

impl ScopeState {
    fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                pending: 0,
                panic: None,
            }),
            idle: Condvar::new(),
        }
    }

    fn is_idle(&self) -> bool {
        lock(&self.inner).pending == 0
    }

    /// Blocks until no job is pending.
    fn block_until_idle(&self) {
        let mut inner = lock(&self.inner);
        while inner.pending > 0 {
            inner = self
                .idle
                .wait(inner)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// Like [`block_until_idle`](Self::block_until_idle), but gives up after `timeout`; returns whether the
    /// scope is idle.
    fn block_until_idle_for(&self, timeout: Duration) -> bool {
        let inner = lock(&self.inner);
        let (inner, _) = self
            .idle
            .wait_timeout_while(inner, timeout, |inner| inner.pending > 0)
            .unwrap_or_else(PoisonError::into_inner);
        inner.pending == 0
    }

    /// Keeps the first panic; later ones are dropped (carefully: dropping a payload may itself panic).
    fn record_panic(&self, payload: PanicPayload) {
        let rejected = {
            let mut inner = lock(&self.inner);
            if inner.panic.is_none() {
                inner.panic = Some(payload);
                None
            } else {
                Some(payload)
            }
        };
        discard(rejected);
    }

    fn take_panic(&self) -> Option<PanicPayload> {
        lock(&self.inner).panic.take()
    }
}

/// Drops a panic payload without letting a panic in its destructor escape.
fn discard(payload: Option<Box<dyn Any + Send>>) {
    let _ = panic::catch_unwind(AssertUnwindSafe(move || drop(payload)));
}

/// Proof that one job of the scope is alive; see invariant 1 in the module documentation.
struct ScopeGuard {
    state: Arc<ScopeState>,
}

impl ScopeGuard {
    fn new(state: &Arc<ScopeState>) -> Self {
        lock(&state.inner).pending += 1;
        Self {
            state: Arc::clone(state),
        }
    }
}

impl Drop for ScopeGuard {
    fn drop(&mut self) {
        let finished = {
            let mut inner = lock(&self.state.inner);
            inner.pending -= 1;
            inner.pending == 0
        };
        if finished {
            self.state.idle.notify_all();
        }
    }
}

/// A job together with its guard. The field order is part of the safety argument (invariant 2): the job drops
/// first.
struct ScopedTask<F> {
    job: F,
    guard: ScopeGuard,
}

/// A handle for spawning jobs that may borrow from the surrounding code. Only
/// [`ThreadPool::scope`](crate::ThreadPool::scope) and [`Spawner::scope`](crate::Spawner::scope) create one,
/// and they hand out nothing but `&Scope`.
///
/// `'env` is the lifetime of the data the jobs may borrow (everything that outlives the `scope` call);
/// `'scope` is the lifetime of the scope itself. A job may be spawned from inside another job through the same
/// `&Scope` it captured.
///
/// What a job cannot do is outlive the scope's body while borrowing something that dies with it. This is
/// rejected at compile time:
///
/// ```compile_fail
/// use threading::{PoolConfig, ThreadPool};
///
/// let pool = ThreadPool::new(PoolConfig::new())?;
/// pool.scope(|scope| {
///     let local = String::from("dies with the closure body");
///     // error: `local` does not live long enough, the job may run until the scope ends
///     scope.spawn(|| println!("{local}"));
/// });
/// # Ok::<(), threading::ThreadingError>(())
/// ```
///
/// Nor can a `&Scope` escape the closure:
///
/// ```compile_fail
/// use threading::{PoolConfig, ThreadPool};
///
/// let pool = ThreadPool::new(PoolConfig::new())?;
/// let mut leaked = None;
/// pool.scope(|scope| leaked = Some(scope));
/// # Ok::<(), threading::ThreadingError>(())
/// ```
///
/// The usual rules of sharing apply across jobs too. Two jobs cannot both borrow a variable mutably:
///
/// ```compile_fail
/// use threading::{PoolConfig, ThreadPool};
///
/// let pool = ThreadPool::new(PoolConfig::new())?;
/// let mut count = 0;
/// pool.scope(|scope| {
///     scope.spawn(|| count += 1);
///     scope.spawn(|| count += 1); // error: second mutable borrow
/// });
/// # Ok::<(), threading::ThreadingError>(())
/// ```
///
/// The body cannot read what a job may be changing:
///
/// ```compile_fail
/// use threading::{PoolConfig, ThreadPool};
///
/// let pool = ThreadPool::new(PoolConfig::new())?;
/// let mut count = 0;
/// pool.scope(|scope| {
///     scope.spawn(|| count += 1);
///     println!("{count}"); // error: `count` is mutably borrowed by the job
/// });
/// # Ok::<(), threading::ThreadingError>(())
/// ```
///
/// A job cannot capture something that is not safe to send to another thread:
///
/// ```compile_fail
/// use std::rc::Rc;
/// use threading::{PoolConfig, ThreadPool};
///
/// let pool = ThreadPool::new(PoolConfig::new())?;
/// let shared = Rc::new(1);
/// pool.scope(|scope| {
///     scope.spawn(|| println!("{shared}")); // error: `Rc` is not `Sync`
/// });
/// # Ok::<(), threading::ThreadingError>(())
/// ```
///
/// And a value moved into a job is gone for the caller:
///
/// ```compile_fail
/// use threading::{PoolConfig, ThreadPool};
///
/// let pool = ThreadPool::new(PoolConfig::new())?;
/// let values = vec![1, 2, 3];
/// pool.scope(|scope| {
///     scope.spawn(move || drop(values));
/// });
/// println!("{values:?}"); // error: borrow of moved value
/// # Ok::<(), threading::ThreadingError>(())
/// ```
pub struct Scope<'scope, 'env: 'scope> {
    shared: Arc<Shared>,
    state: Arc<ScopeState>,
    // Invariant in both lifetimes, exactly as in `std::thread::Scope`.
    scope: PhantomData<&'scope mut &'scope ()>,
    env: PhantomData<&'env mut &'env ()>,
}

impl<'scope> Scope<'scope, '_> {
    /// Queues a job with [`JobPriority::Normal`] that may borrow from the surrounding code. The scope waits
    /// for it before [`scope`](crate::ThreadPool::scope) returns.
    ///
    /// The job returns nothing; hand results back through the data it borrows (a `&mut` to a slot or element,
    /// an atomic, a `Mutex`) or use [`join`](crate::ThreadPool::join). If the job panics, the other jobs still
    /// finish and the scope then re-raises the first panic.
    pub fn spawn<F>(&'scope self, job: F)
    where
        F: FnOnce() + Send + 'scope,
    {
        self.spawn_with_priority(JobPriority::Normal, job);
    }

    /// Like [`spawn`](Self::spawn), with the given priority.
    pub fn spawn_with_priority<F>(&'scope self, priority: JobPriority, job: F)
    where
        F: FnOnce() + Send + 'scope,
    {
        let task = self.erased_task(job);

        if let Err(task) = self.shared.submit(priority, task) {
            // The pool is shutting down or has shut down and this thread is not running one of its jobs (only
            // possible through a `Spawner` that outlived the pool), so the job cannot be queued: run it here.
            // The scope's contract holds either way.
            task();
        }
    }

    /// Like [`spawn_with_priority`](Self::spawn_with_priority), but the owner can **take the job back** with
    /// [`Claim::run_here`] if nobody has started it by then, and run it on its own thread.
    ///
    /// This is what [`join`](crate::parallel) is built on. A job that is taken back leaves behind in the queue
    /// only an empty shell, and the scope does not have to wait for that shell: the job's guard is gone as soon
    /// as the job has run, so `pending` is 0 without anybody popping the shell. Without it the owner would
    /// wait, and help with whatever the queues offer (the oldest job, which is some enclosing split), nesting
    /// work inside work: the stack depth then grows with the number of jobs instead of with the recursion.
    pub(crate) fn spawn_claimable<F>(&'scope self, priority: JobPriority, job: F) -> Claim
    where
        F: FnOnce() + Send + 'scope,
    {
        // The erased task (job and guard) lives in a cell that holds only `'static` types, and the queue gets
        // a `'static` shell that takes the task out of the cell when it runs. Whoever takes the task out first
        // (the shell, or the owner through the claim) runs it; the other finds the cell empty and does nothing.
        // The task is accounted for by its guard in either case (invariant 1), and the shell, which can
        // outlive the scope, holds no borrowed data once the task is out of the cell.
        let task = self.erased_task(job);
        let cell: Arc<Mutex<Option<Task>>> = Arc::new(Mutex::new(Some(task)));
        let queued = Arc::clone(&cell);
        let shell: Task = Box::new(move || {
            let task = lock(&queued).take();
            if let Some(task) = task {
                task();
            }
        });
        if let Err(shell) = self.shared.submit(priority, shell) {
            // The pool refuses the job (see `spawn_with_priority`): run it here.
            shell();
        }
        Claim { cell }
    }

    /// Wraps `job` into a `'static` task that owns a guard of this scope (invariants 1 and 2).
    fn erased_task<F>(&'scope self, job: F) -> Task
    where
        F: FnOnce() + Send + 'scope,
    {
        // The task is boxed so that the closure below captures only a `Box`, not the job itself (invariant 7):
        // the job moves onto the stack inside the closure and is consumed by its call before the guard drops.
        let scoped = Box::new(ScopedTask {
            job,
            guard: ScopeGuard::new(&self.state),
        });
        let task: Box<dyn FnOnce() + Send + 'scope> = Box::new(move || {
            let ScopedTask { job, guard } = *scoped;
            if let Err(payload) = panic::catch_unwind(AssertUnwindSafe(job)) {
                guard.state.record_panic(payload);
            }
            // `guard` drops here, after the job has been consumed (invariant 2).
        });

        // SAFETY: the task owns a `ScopeGuard` of this scope, and `run_scope` does not return before every
        // such guard is dropped (invariants 1 and 3), so the task, and with it everything of lifetime
        // `'scope` that it holds, is dropped or finished before `'scope` ends.
        unsafe { erase_lifetime(task) }
    }
}

/// The owner's way back to a job queued with [`Scope::spawn_claimable`].
///
/// Dropping a claim does nothing: the job stays queued and runs when somebody pops it (a scope that unwinds
/// before it takes the job back still has it run by the thread that waits for the scope).
pub(crate) struct Claim {
    cell: Arc<Mutex<Option<Task>>>,
}

impl Claim {
    /// Runs the job on the calling thread if nobody has started it yet; does nothing if it has been started
    /// (it is running, or done, on another thread).
    pub(crate) fn run_here(self) {
        let task = lock(&self.cell).take();
        if let Some(task) = task {
            task();
        }
    }
}

impl fmt::Debug for Scope<'_, '_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Scope")
            .field("idle", &self.state.is_idle())
            .finish()
    }
}

/// Hides the lifetime bound of a job so that it fits the pool's `'static` queues.
///
/// # Safety
///
/// The caller must make sure that the returned task has finished running, or has been dropped, before the
/// data of lifetime `'a` that it holds goes away.
unsafe fn erase_lifetime<'a>(task: Box<dyn FnOnce() + Send + 'a>) -> Task {
    // SAFETY: both types are the same fat pointer; only the lifetime bound of the trait object differs. The
    // caller upholds the contract above.
    unsafe {
        mem::transmute::<Box<dyn FnOnce() + Send + 'a>, Box<dyn FnOnce() + Send + 'static>>(task)
    }
}

/// Aborts the process if dropped while unwinding. Held across the wait in [`run_scope`]: unwinding out of the
/// wait would let the stack frame, which jobs may still borrow from, go away.
struct AbortOnUnwind;

impl Drop for AbortOnUnwind {
    fn drop(&mut self) {
        if thread::panicking() {
            std::process::abort();
        }
    }
}

/// Runs queued jobs on the calling thread until the scope has no job left.
fn wait_until_idle(shared: &Shared, state: &ScopeState) {
    loop {
        if state.is_idle() {
            return;
        }
        if shared.run_one() {
            continue;
        }
        if shared.has_queued() {
            // A job is being pushed right now (its counter is raised a moment before it becomes visible).
            thread::yield_now();
            continue;
        }
        // Nothing to run: the remaining jobs are running on other threads, and the last one to finish wakes us.
        if shared.worker_count() == 0 {
            // Without workers nobody runs what those jobs spawn next once their thread is gone: look again soon.
            if state.block_until_idle_for(IDLE_POLL) {
                return;
            }
            continue;
        }
        state.block_until_idle();
        return;
    }
}

/// Runs `body` with a [`Scope`], waits for every job spawned in it, and returns the body's value.
///
/// If the body panics, the jobs are waited for first and the body's panic is re-raised. Otherwise, if a job
/// panicked, the first such panic is re-raised after all jobs finished.
pub(crate) fn run_scope<'env, F, T>(shared: &Arc<Shared>, body: F) -> T
where
    F: for<'scope> FnOnce(&'scope Scope<'scope, 'env>) -> T,
{
    let state = Arc::new(ScopeState::new());
    let scope = Scope {
        shared: Arc::clone(shared),
        state: Arc::clone(&state),
        scope: PhantomData,
        env: PhantomData,
    };

    let outcome = panic::catch_unwind(AssertUnwindSafe(|| body(&scope)));

    let abort_if_this_unwinds = AbortOnUnwind;
    wait_until_idle(shared, &state);
    mem::forget(abort_if_this_unwinds);

    let job_panic = state.take_panic();
    match outcome {
        Err(body_panic) => {
            discard(job_panic);
            panic::resume_unwind(body_panic)
        }
        Ok(value) => match job_panic {
            Some(payload) => panic::resume_unwind(payload),
            None => value,
        },
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::{PoolConfig, ThreadPool, WorkerCount};

    fn pool(workers: usize) -> ThreadPool {
        ThreadPool::new(PoolConfig::new().with_workers(WorkerCount::Exact(workers))).unwrap()
    }

    #[test]
    fn a_claimed_job_runs_on_the_owner_and_the_scope_does_not_wait_for_the_shell() {
        // No workers: nobody else can start the job, so the claim runs it here, and the scope is idle at once,
        // although the queue still holds the empty shell.
        let pool = pool(0);
        let runs = AtomicUsize::new(0);
        let (queued_after, pending_after) = run_scope(&pool.shared, |scope| {
            let claim = scope.spawn_claimable(JobPriority::Normal, || {
                runs.fetch_add(1, Ordering::SeqCst);
            });
            claim.run_here();
            (pool.shared.has_queued(), scope.state.is_idle())
        });
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        assert!(queued_after, "the shell is still queued");
        assert!(pending_after, "but the scope does not count it");
        // The shell is harmless: running it does nothing.
        pool.run_pending();
        assert_eq!(runs.load(Ordering::SeqCst), 1, "the job ran exactly once");
    }

    #[test]
    fn a_job_that_somebody_else_started_is_not_run_again_by_the_claim() {
        let pool = pool(0);
        let runs = AtomicUsize::new(0);
        run_scope(&pool.shared, |scope| {
            let claim = scope.spawn_claimable(JobPriority::Normal, || {
                runs.fetch_add(1, Ordering::SeqCst);
            });
            // Somebody pops the shell first (here: this thread, standing in for a worker or a helper).
            assert!(pool.shared.run_one());
            assert_eq!(runs.load(Ordering::SeqCst), 1);
            claim.run_here();
            assert_eq!(
                runs.load(Ordering::SeqCst),
                1,
                "the claim found the cell empty"
            );
        });
    }

    #[test]
    fn a_dropped_claim_leaves_the_job_to_the_scope() {
        let pool = pool(0);
        let runs = AtomicUsize::new(0);
        run_scope(&pool.shared, |scope| {
            drop(scope.spawn_claimable(JobPriority::Normal, || {
                runs.fetch_add(1, Ordering::SeqCst);
            }));
            assert_eq!(runs.load(Ordering::SeqCst), 0, "not run yet");
        });
        assert_eq!(
            runs.load(Ordering::SeqCst),
            1,
            "the scope ran it before it returned"
        );
    }

    #[test]
    fn a_panic_in_a_claimed_job_is_reported_by_the_scope() {
        let pool = pool(0);
        let failure = panic::catch_unwind(AssertUnwindSafe(|| {
            run_scope(&pool.shared, |scope| {
                scope
                    .spawn_claimable(JobPriority::Normal, || panic!("claimed"))
                    .run_here();
            });
        }))
        .unwrap_err();
        assert_eq!(failure.downcast_ref::<&str>(), Some(&"claimed"));
    }

    #[test]
    fn the_guard_counts_up_and_down() {
        let state = Arc::new(ScopeState::new());
        assert!(state.is_idle());
        let first = ScopeGuard::new(&state);
        let second = ScopeGuard::new(&state);
        assert!(!state.is_idle());
        drop(first);
        assert!(!state.is_idle());
        drop(second);
        assert!(state.is_idle());
    }

    #[test]
    fn only_the_first_panic_is_kept() {
        let state = ScopeState::new();
        state.record_panic(Box::new("first"));
        state.record_panic(Box::new("second"));
        let kept = state.take_panic().expect("a panic was recorded");
        assert_eq!(kept.downcast_ref::<&str>(), Some(&"first"));
        assert!(state.take_panic().is_none());
    }

    #[test]
    fn a_task_that_is_dropped_without_running_still_releases_the_scope() {
        // The pool never does this, but invariant 1 must not depend on it.
        let state = Arc::new(ScopeState::new());
        let scoped = ScopedTask {
            job: || (),
            guard: ScopeGuard::new(&state),
        };
        assert!(!state.is_idle());
        drop(scoped);
        assert!(state.is_idle());
    }

    #[test]
    fn the_job_is_dropped_before_the_scope_is_released() {
        struct Probe<'a>(&'a ScopeState, &'a AtomicUsize);
        impl Drop for Probe<'_> {
            fn drop(&mut self) {
                // While the job's captures are dropped, the scope must still count the job as pending.
                self.1
                    .store(usize::from(!self.0.is_idle()), Ordering::SeqCst);
            }
        }
        let state = Arc::new(ScopeState::new());
        let seen_pending = AtomicUsize::new(0);
        let probe = Probe(&state, &seen_pending);
        let scoped = ScopedTask {
            job: move || drop(probe),
            guard: ScopeGuard::new(&state),
        };
        drop(scoped);
        assert_eq!(seen_pending.load(Ordering::SeqCst), 1);
        assert!(state.is_idle());
    }

    #[test]
    fn the_caller_may_read_what_a_job_wrote_through_a_mutable_borrow_as_soon_as_the_scope_ends() {
        // Regression test for a Miri (Stacked Borrows) error: the scope used to be released while the task's
        // closure, which owned the job's `&mut`, had not returned yet. Run it many times on a pool with workers
        // so that the job usually runs on another thread.
        let pool = pool(2);
        for round in 0..50_usize {
            let mut slot = 0;
            pool.scope(|scope| {
                scope.spawn(|| slot = round + 1);
            });
            assert_eq!(slot, round + 1);
        }
    }

    #[test]
    fn spawning_into_a_closed_pool_runs_the_job_inline() {
        let pool = pool(0);
        let spawner = pool.spawner();
        drop(pool);
        let ran = AtomicUsize::new(0);
        spawner.scope(|scope| {
            scope.spawn(|| {
                ran.fetch_add(1, Ordering::SeqCst);
            });
        });
        assert_eq!(ran.load(Ordering::SeqCst), 1);
    }
}
