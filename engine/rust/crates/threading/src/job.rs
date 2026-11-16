//! Job handles: how the result of a job (or its panic) gets back to the code that spawned it.

use std::any::Any;
use std::fmt;
use std::mem;
use std::panic;
use std::sync::{Arc, Condvar, Mutex};

use crate::pool::Shared;
use crate::sync::lock;

/// The payload of a panic, as `catch_unwind` hands it out.
pub(crate) type PanicPayload = Box<dyn Any + Send + 'static>;

/// Where the result of one job is stored until somebody waits for it.
pub(crate) struct Slot<T> {
    state: Mutex<SlotState<T>>,
    ready: Condvar,
}

enum SlotState<T> {
    /// The job has not finished.
    Pending,
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

    /// Stores the result and wakes whoever waits for it.
    pub(crate) fn complete(&self, result: Result<T, PanicPayload>) {
        *lock(&self.state) = SlotState::Ready(result);
        self.ready.notify_all();
    }

    fn is_done(&self) -> bool {
        !matches!(*lock(&self.state), SlotState::Pending)
    }

    /// Takes the result if the job is done, without blocking.
    fn try_take(&self) -> Option<Result<T, PanicPayload>> {
        let mut state = lock(&self.state);
        match mem::replace(&mut *state, SlotState::Taken) {
            SlotState::Ready(result) => Some(result),
            SlotState::Pending => {
                *state = SlotState::Pending;
                None
            }
            SlotState::Taken => None,
        }
    }

    /// Blocks until the job is done and takes the result.
    fn take_blocking(&self) -> Result<T, PanicPayload> {
        let mut state = lock(&self.state);
        while matches!(*state, SlotState::Pending) {
            state = self
                .ready
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        match mem::replace(&mut *state, SlotState::Taken) {
            SlotState::Ready(result) => result,
            SlotState::Pending | SlotState::Taken => {
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
                return self.slot.take_blocking().map_err(JobPanic::new);
            }
        }
    }
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
