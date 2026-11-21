//! Job handles: how the result of a job (or its panic) gets back to the code that spawned it.

use std::any::Any;
use std::fmt;
use std::mem;
use std::panic::{self, AssertUnwindSafe};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Duration;

use crate::pool::{Shared, Spawner};
use crate::priority::JobPriority;
use crate::queue::Task;
use crate::sync::{IDLE_POLL, lock};

/// The payload of a panic, as `catch_unwind` hands it out.
pub(crate) type PanicPayload = Box<dyn Any + Send + 'static>;

/// Code that takes over the result of a job when it finishes, instead of a waiting thread (see
/// [`Slot::on_complete`]). It runs on whichever thread finishes the job, outside every lock, and must not
/// panic.
pub(crate) type Continuation<T> = Box<dyn FnOnce(Result<T, PanicPayload>) + Send + 'static>;

/// Drops a panic payload without letting a panic in its destructor escape.
pub(crate) fn discard_payload(payload: PanicPayload) {
    let _ = panic::catch_unwind(AssertUnwindSafe(move || drop(payload)));
}

/// Where the result of one job is stored until somebody waits for it.
pub(crate) struct Slot<T> {
    state: Mutex<SlotState<T>>,
    ready: Condvar,
}

enum SlotState<T> {
    /// The job has not finished.
    Pending,
    /// The job has not finished, and `continuation` takes over its result when it does. The result never
    /// reaches the slot.
    Continued(Continuation<T>),
    /// The job finished; nobody has taken the result yet.
    Ready(Result<T, PanicPayload>),
    /// The result has been taken. Cannot be seen through a [`JobHandle`], which is consumed by the wait that
    /// takes it.
    Taken,
}

impl<T> Slot<T> {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(SlotState::Pending),
            ready: Condvar::new(),
        }
    }

    /// Stores the result and wakes whoever waits for it, or hands it to the continuation registered with
    /// [`on_complete`](Self::on_complete).
    pub(crate) fn complete(&self, result: Result<T, PanicPayload>) {
        let mut state = lock(&self.state);
        match mem::replace(&mut *state, SlotState::Taken) {
            SlotState::Pending => {
                *state = SlotState::Ready(result);
                drop(state);
                self.ready.notify_all();
            }
            SlotState::Continued(continuation) => {
                // The state stays `Taken`: the result has been passed on.
                drop(state);
                continuation(result);
            }
            SlotState::Ready(_) | SlotState::Taken => {
                unreachable!("a job completes exactly once")
            }
        }
    }

    /// Registers `continuation` to take over the result. If the job is done already the continuation runs
    /// right now on this thread, otherwise on the thread that completes the job. Either way it runs once,
    /// outside every lock.
    ///
    /// Only for a slot nobody else waits on: the handle that owned it has been consumed.
    pub(crate) fn on_complete(&self, continuation: Continuation<T>) {
        let mut state = lock(&self.state);
        match mem::replace(&mut *state, SlotState::Taken) {
            SlotState::Pending => *state = SlotState::Continued(continuation),
            SlotState::Ready(result) => {
                drop(state);
                continuation(result);
            }
            SlotState::Continued(_) | SlotState::Taken => {
                unreachable!("a handle is consumed by the call that registers a continuation")
            }
        }
    }

    fn is_done(&self) -> bool {
        matches!(*lock(&self.state), SlotState::Ready(_) | SlotState::Taken)
    }

    /// Takes the result if the job is done, without blocking.
    fn try_take(&self) -> Option<Result<T, PanicPayload>> {
        let mut state = lock(&self.state);
        match mem::replace(&mut *state, SlotState::Taken) {
            SlotState::Ready(result) => Some(result),
            unfinished @ (SlotState::Pending | SlotState::Continued(_)) => {
                *state = unfinished;
                None
            }
            SlotState::Taken => None,
        }
    }

    /// Blocks until the job is done and takes the result.
    /// Like [`take_blocking`](Self::take_blocking), but gives up after `timeout` (`None`).
    fn take_for(&self, timeout: Duration) -> Option<Result<T, PanicPayload>> {
        let state = lock(&self.state);
        let (mut state, _) = self
            .ready
            .wait_timeout_while(state, timeout, |state| {
                matches!(*state, SlotState::Pending | SlotState::Continued(_))
            })
            .unwrap_or_else(PoisonError::into_inner);
        if matches!(*state, SlotState::Pending | SlotState::Continued(_)) {
            return None;
        }
        match mem::replace(&mut *state, SlotState::Taken) {
            SlotState::Ready(result) => Some(result),
            SlotState::Pending | SlotState::Continued(_) | SlotState::Taken => {
                unreachable!("only a finished job gets here, and a handle is consumed by its wait")
            }
        }
    }

    fn take_blocking(&self) -> Result<T, PanicPayload> {
        let mut state = lock(&self.state);
        while matches!(*state, SlotState::Pending | SlotState::Continued(_)) {
            state = self
                .ready
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        match mem::replace(&mut *state, SlotState::Taken) {
            SlotState::Ready(result) => result,
            SlotState::Pending | SlotState::Continued(_) | SlotState::Taken => {
                unreachable!(
                    "the loop above only ends on a finished job, and a handle is consumed by its wait"
                )
            }
        }
    }
}

/// A job that panicked: the panic payload, kept so the waiting side can inspect or re-raise it.
///
/// Returned by [`JobHandle::wait_catch`]; [`JobHandle::wait`] re-raises the same panic on the waiting thread
/// instead.
pub struct JobPanic {
    payload: PanicPayload,
}

impl JobPanic {
    pub(crate) fn new(payload: PanicPayload) -> Self {
        Self { payload }
    }

    /// The panic message, when the job panicked with a string (which is what `panic!("...")` and
    /// `panic!("{x}")` produce). `None` for a panic with any other payload type.
    #[must_use]
    pub fn message(&self) -> Option<&str> {
        if let Some(text) = self.payload.downcast_ref::<&'static str>() {
            Some(text)
        } else {
            self.payload.downcast_ref::<String>().map(String::as_str)
        }
    }

    /// Gives back the raw payload, for [`std::panic::resume_unwind`] or for downcasting to a custom type.
    #[must_use]
    pub fn into_payload(self) -> Box<dyn Any + Send + 'static> {
        self.payload
    }
}

impl fmt::Debug for JobPanic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JobPanic")
            .field("message", &self.message())
            .finish()
    }
}

impl fmt::Display for JobPanic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.message() {
            Some(message) => write!(f, "job panicked: {message}"),
            None => f.write_str("job panicked with a non-string payload"),
        }
    }
}

impl std::error::Error for JobPanic {}

/// The way to get the result of a job started with [`ThreadPool::spawn`](crate::ThreadPool::spawn).
///
/// Waiting consumes the handle ([`wait`](Self::wait), [`wait_catch`](Self::wait_catch)), so a result is taken
/// exactly once. The handle does not borrow the pool and may outlive it: a pool finishes every queued job
/// before it shuts down, so a handle never waits for something that will not happen.
///
/// Dropping a handle without waiting does not cancel the job. It still runs, and its result (or panic
/// payload) is discarded. A panic still prints its message through the panic hook, like any panic.
///
/// A thread that waits does not sit idle while jobs are queued: it runs queued jobs itself until its own job
/// is finished. That keeps a job that waits for another job from deadlocking a pool with few workers, and it
/// is what makes a pool with no workers ([`WorkerCount::Exact(0)`](crate::WorkerCount::Exact)) work at all.
/// The cost is that a waiting thread may run an unrelated job before it returns.
#[must_use = "a job handle is the only way to get the result; drop it only for fire-and-forget jobs"]
pub struct JobHandle<T> {
    slot: Arc<Slot<T>>,
    shared: Arc<Shared>,
}

impl<T> JobHandle<T> {
    pub(crate) fn new(slot: Arc<Slot<T>>, shared: Arc<Shared>) -> Self {
        Self { slot, shared }
    }

    /// Whether the job has finished (successfully or by panicking). A `true` stays `true`.
    #[must_use]
    pub fn is_done(&self) -> bool {
        self.slot.is_done()
    }

    /// Waits for the job and returns its result.
    ///
    /// # Panics
    ///
    /// Re-raises the job's panic on the calling thread (with the original payload) if the job panicked. Use
    /// [`wait_catch`](Self::wait_catch) to get the panic as a value instead.
    pub fn wait(self) -> T {
        match self.wait_catch() {
            Ok(value) => value,
            Err(panic) => panic::resume_unwind(panic.into_payload()),
        }
    }

    /// Waits for the job and returns its result, or the job's panic as a [`JobPanic`].
    ///
    /// # Errors
    ///
    /// [`JobPanic`] when the job panicked.
    pub fn wait_catch(self) -> Result<T, JobPanic> {
        loop {
            if let Some(result) = self.slot.try_take() {
                return result.map_err(JobPanic::new);
            }
            // Not done: run somebody's job instead of sleeping (see the type's documentation).
            if !self.shared.run_one() {
                // The queue is empty, so the job is running on another thread and will wake us.
                if self.shared.worker_count() == 0 {
                    // No worker will run what that thread queues next (a continuation): look again soon.
                    if let Some(result) = self.slot.take_for(IDLE_POLL) {
                        return result.map_err(JobPanic::new);
                    }
                    continue;
                }
                return self.slot.take_blocking().map_err(JobPanic::new);
            }
        }
    }
}

impl<T: Send + 'static> JobHandle<T> {
    /// Queues `next` to run with this job's result once the job has finished, and returns the handle for
    /// `next`'s result. The job's handle is consumed.
    ///
    /// Nothing waits: no thread is blocked while the first job runs, and `next` is queued by whichever thread
    /// finishes it (or right away, when the job is done already). `next` is queued with
    /// [`JobPriority::Normal`]; see [`then_with_priority`](Self::then_with_priority).
    ///
    /// If the job panics, `next` does not run: the panic travels on to the new handle, which re-raises it (or
    /// returns it as a [`JobPanic`]) exactly like the original handle would have. Chains therefore carry the
    /// first failure to the end.
    ///
    /// ```
    /// use threading::{PoolConfig, ThreadPool};
    ///
    /// let pool = ThreadPool::new(PoolConfig::new())?;
    /// let text = pool
    ///     .spawn(|| 20)
    ///     .then(|n| n + 1)
    ///     .then(|n| format!("the answer is {}", n * 2));
    /// assert_eq!(text.wait(), "the answer is 42");
    /// # Ok::<(), threading::ThreadingError>(())
    /// ```
    ///
    /// With a pool without workers ([`WorkerCount::Exact(0)`](crate::WorkerCount::Exact)) the continuation
    /// is queued like any other job: it runs on the thread that waits for it or calls
    /// [`run_pending`](crate::ThreadPool::run_pending). A thread that waits on such a pool while another thread
    /// runs the job it needs looks at the queue again every millisecond, so it finds the continuation that
    /// thread queues.
    pub fn then<F, U>(self, next: F) -> JobHandle<U>
    where
        F: FnOnce(T) -> U + Send + 'static,
        U: Send + 'static,
    {
        self.then_with_priority(JobPriority::Normal, next)
    }

    /// Like [`then`](Self::then), with the priority of the continuation.
    pub fn then_with_priority<F, U>(self, priority: JobPriority, next: F) -> JobHandle<U>
    where
        F: FnOnce(T) -> U + Send + 'static,
        U: Send + 'static,
    {
        let target = Arc::clone(&self.shared);
        self.then_in(target, priority, next)
    }

    /// Like [`then`](Self::then), but the continuation is queued on **another pool**, the one of `spawner`.
    ///
    /// This is how work moves between pools: read a file on the IO pool, then decode it on the compute pool,
    /// without a thread of either pool waiting for the other.
    ///
    /// ```
    /// use threading::{PoolConfig, ThreadPool};
    ///
    /// let io = ThreadPool::new(PoolConfig::io())?;
    /// let compute = ThreadPool::new(PoolConfig::new())?;
    /// let decoded = io
    ///     .spawn(|| vec![1_u8, 2, 3]) // "reads a file" on an IO worker
    ///     .then_on(&compute.spawner(), |bytes| {
    ///         // "decodes it" on the compute pool
    ///         bytes.iter().map(|&b| u32::from(b)).sum::<u32>()
    ///     });
    /// assert_eq!(decoded.wait(), 6);
    /// # Ok::<(), threading::ThreadingError>(())
    /// ```
    ///
    /// The returned handle belongs to the target pool: a thread that waits for it helps that pool. If the
    /// target pool is shutting down and the finishing thread is not one of its jobs, the continuation runs on
    /// that thread instead of being dropped, as for [`then`](Self::then).
    pub fn then_on<F, U>(self, spawner: &Spawner, next: F) -> JobHandle<U>
    where
        F: FnOnce(T) -> U + Send + 'static,
        U: Send + 'static,
    {
        self.then_on_with_priority(spawner, JobPriority::Normal, next)
    }

    /// Like [`then_on`](Self::then_on), with the priority of the continuation on the target pool.
    pub fn then_on_with_priority<F, U>(
        self,
        spawner: &Spawner,
        priority: JobPriority,
        next: F,
    ) -> JobHandle<U>
    where
        F: FnOnce(T) -> U + Send + 'static,
        U: Send + 'static,
    {
        self.then_in(Arc::clone(&spawner.shared), priority, next)
    }

    /// The common part of the `then` family: queues the continuation on `target`.
    fn then_in<F, U>(self, target: Arc<Shared>, priority: JobPriority, next: F) -> JobHandle<U>
    where
        F: FnOnce(T) -> U + Send + 'static,
        U: Send + 'static,
    {
        let output = Arc::new(Slot::new());
        let handle = JobHandle::new(Arc::clone(&output), Arc::clone(&target));
        self.slot.on_complete(Box::new(move |result| match result {
            Err(payload) => output.complete(Err(payload)),
            Ok(value) => {
                let task: Task = Box::new(move || {
                    let result = panic::catch_unwind(AssertUnwindSafe(move || next(value)));
                    output.complete(result);
                });
                target.submit_or_run(priority, task);
            }
        }));
        handle
    }
}

/// What [`join_all`] collects while its jobs finish.
struct Gather<T> {
    state: Mutex<GatherState<T>>,
    output: Arc<Slot<Vec<T>>>,
}

struct GatherState<T> {
    /// One entry per input job, in input order; `None` until that job has finished.
    results: Vec<Option<Result<T, PanicPayload>>>,
    /// How many jobs have not finished yet. The job that brings it to 0 completes the output.
    remaining: usize,
}

impl<T> Gather<T> {
    fn finish(&self, index: usize, result: Result<T, PanicPayload>) {
        let finished = {
            let mut state = lock(&self.state);
            state.results[index] = Some(result);
            state.remaining -= 1;
            if state.remaining == 0 {
                Some(mem::take(&mut state.results))
            } else {
                None
            }
        };
        let Some(results) = finished else { return };

        let mut values = Vec::with_capacity(results.len());
        let mut first_panic: Option<PanicPayload> = None;
        for result in results.into_iter().flatten() {
            match result {
                Ok(value) => values.push(value),
                Err(payload) if first_panic.is_none() => first_panic = Some(payload),
                Err(payload) => discard_payload(payload),
            }
        }
        match first_panic {
            Some(payload) => {
                drop(values);
                self.output.complete(Err(payload));
            }
            None => self.output.complete(Ok(values)),
        }
    }
}

/// A job that finishes when every job of `handles` has finished: the counter-based dependency that
/// [`ThreadPool::when_all`](crate::ThreadPool::when_all) is made of. Nothing waits and no job is queued: the
/// last of the jobs to finish completes the returned handle.
pub(crate) fn join_all<T>(shared: &Arc<Shared>, handles: Vec<JobHandle<T>>) -> JobHandle<Vec<T>>
where
    T: Send + 'static,
{
    let output = Arc::new(Slot::new());
    let joined = JobHandle::new(Arc::clone(&output), Arc::clone(shared));
    if handles.is_empty() {
        output.complete(Ok(Vec::new()));
        return joined;
    }

    let gather = Arc::new(Gather {
        state: Mutex::new(GatherState {
            results: handles.iter().map(|_| None).collect(),
            remaining: handles.len(),
        }),
        output,
    });
    for (index, handle) in handles.into_iter().enumerate() {
        let gather = Arc::clone(&gather);
        handle
            .slot
            .on_complete(Box::new(move |result| gather.finish(index, result)));
    }
    joined
}

impl<T> fmt::Debug for JobHandle<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JobHandle")
            .field("done", &self.is_done())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn panic_with<T: Any + Send>(value: T) -> JobPanic {
        JobPanic::new(Box::new(value))
    }

    #[test]
    fn a_str_panic_has_a_message() {
        let panic = panic_with("boom");
        assert_eq!(panic.message(), Some("boom"));
        assert_eq!(panic.to_string(), "job panicked: boom");
    }

    #[test]
    fn a_string_panic_has_a_message() {
        let panic = panic_with(String::from("formatted 42"));
        assert_eq!(panic.message(), Some("formatted 42"));
    }

    #[test]
    fn another_payload_has_no_message_but_can_be_recovered() {
        let panic = panic_with(42_u32);
        assert_eq!(panic.message(), None);
        assert_eq!(panic.to_string(), "job panicked with a non-string payload");
        assert_eq!(panic.into_payload().downcast_ref::<u32>(), Some(&42));
    }

    #[test]
    fn debug_shows_the_message() {
        let text = format!("{:?}", panic_with("boom"));
        assert!(text.contains("boom"), "{text}");
    }

    #[test]
    fn a_continuation_registered_first_gets_the_result_when_the_job_completes() {
        let slot = Slot::<u32>::new();
        let seen = Arc::new(Mutex::new(None));
        let sink = Arc::clone(&seen);
        slot.on_complete(Box::new(move |result| {
            *sink.lock().unwrap() = Some(result.ok());
        }));
        assert!(!slot.is_done(), "a continued slot is not done");
        assert!(
            slot.try_take().is_none(),
            "nobody can take a continued result"
        );
        assert!(seen.lock().unwrap().is_none());
        slot.complete(Ok(9));
        assert_eq!(*seen.lock().unwrap(), Some(Some(9)));
        assert!(slot.is_done());
    }

    #[test]
    fn a_continuation_registered_late_runs_at_once_with_the_result() {
        let slot = Slot::<u32>::new();
        slot.complete(Err(Box::new("late")));
        let seen = Arc::new(Mutex::new(None));
        let sink = Arc::clone(&seen);
        slot.on_complete(Box::new(move |result| {
            *sink.lock().unwrap() = Some(
                result
                    .err()
                    .and_then(|p| p.downcast_ref::<&str>().map(|text| (*text).to_owned())),
            );
        }));
        assert_eq!(*seen.lock().unwrap(), Some(Some("late".to_owned())));
    }

    #[test]
    fn a_slot_is_pending_until_completed_and_then_yields_once() {
        let slot = Slot::<u32>::new();
        assert!(!slot.is_done());
        assert!(slot.try_take().is_none());
        assert!(
            !slot.is_done(),
            "a failed try_take must not change the state"
        );
        slot.complete(Ok(7));
        assert!(slot.is_done());
        assert!(matches!(slot.try_take(), Some(Ok(7))));
        assert!(slot.is_done(), "a taken slot still counts as done");
    }
}
