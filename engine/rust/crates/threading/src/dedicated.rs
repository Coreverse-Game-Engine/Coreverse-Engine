//! Dedicated threads: named threads with a role, a priority and a controlled lifetime, for work that must not
//! share a pool (the render thread, the audio thread, a file watcher).
//!
//! A dedicated thread is not a worker: it runs one closure, usually a loop, until the closure returns. What
//! the pool gives its workers (a queue, stealing, help while waiting) is not there; what is there instead is a
//! way to ask the thread to finish ([`StopToken`]), a result you can collect ([`DedicatedThread::join`]) and
//! a rule for what happens when you forget ([`DedicatedThread`]'s `Drop`).

use std::fmt;
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use platform::thread::ThreadPriority;

use crate::error::{ThreadingError, ThreadingResult};
use crate::job::{JobPanic, PanicPayload, discard_payload};
use crate::role::{self, ThreadRole};

/// The settings of a [`DedicatedThread`]. Built with the chained setters, like
/// [`PoolConfig`](crate::PoolConfig).
///
/// ```
/// use threading::{ThreadConfig, ThreadPriority, ThreadRole};
///
/// let config = ThreadConfig::new("cv-render")
///     .with_role(ThreadRole::Render)
///     .with_priority(ThreadPriority::High)
///     .with_stack_size(4 * 1024 * 1024);
/// assert_eq!(config.name(), "cv-render");
/// assert_eq!(config.role(), ThreadRole::Render);
/// ```
#[derive(Clone, Debug)]
pub struct ThreadConfig {
    name: String,
    role: ThreadRole,
    priority: ThreadPriority,
    stack_size: Option<usize>,
}

impl ThreadConfig {
    /// A config for a thread with this name, the role [`ThreadRole::Unknown`], normal priority and the
    /// default stack size. The OS may shorten the name (Linux keeps 15 bytes).
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            role: ThreadRole::Unknown,
            priority: ThreadPriority::Normal,
            stack_size: None,
        }
    }

    /// Sets the role the thread gets (see [`ThreadRole`]).
    #[must_use]
    pub fn with_role(mut self, role: ThreadRole) -> Self {
        self.role = role;
        self
    }

    /// Sets the OS priority the thread asks for when it starts. A request, as for the workers of a pool: when
    /// the OS refuses, the thread keeps the default priority.
    #[must_use]
    pub fn with_priority(mut self, priority: ThreadPriority) -> Self {
        self.priority = priority;
        self
    }

    /// Sets the stack size in bytes (the OS may round it up). Without it the thread gets the standard
    /// library's default.
    #[must_use]
    pub fn with_stack_size(mut self, bytes: usize) -> Self {
        self.stack_size = Some(bytes);
        self
    }

    /// The thread name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The role of the thread.
    #[must_use]
    pub fn role(&self) -> ThreadRole {
        self.role
    }

    /// The priority the thread asks for.
    #[must_use]
    pub fn priority(&self) -> ThreadPriority {
        self.priority
    }

    /// The stack size, if one was set.
    #[must_use]
    pub fn stack_size(&self) -> Option<usize> {
        self.stack_size
    }

    /// Checks the settings without starting anything.
    ///
    /// # Errors
    ///
    /// [`ThreadingError::InvalidConfig`] when the name is empty or contains a NUL character, or the stack size
    /// is 0.
    pub fn validate(&self) -> ThreadingResult<()> {
        if self.name.is_empty() {
            return Err(ThreadingError::InvalidConfig {
                reason: "the thread name is empty",
            });
        }
        if self.name.contains('\0') {
            return Err(ThreadingError::InvalidConfig {
                reason: "the thread name contains a NUL character",
            });
        }
        if self.stack_size == Some(0) {
            return Err(ThreadingError::InvalidConfig {
                reason: "the stack size is 0",
            });
        }
        Ok(())
    }
}

struct StopState {
    stopped: AtomicBool,
    /// Held only to make "check the flag, then sleep" atomic with respect to `request_stop`.
    lock: Mutex<()>,
    wake: Condvar,
}

/// A request to stop, shared between a thread and whoever controls it. Cloning gives another handle to the
/// same request.
///
/// A dedicated thread's closure gets one and polls it; a loop that has nothing to do sleeps on it so that a
/// stop request wakes it at once:
///
/// ```
/// use std::time::Duration;
/// use threading::{DedicatedThread, ThreadConfig};
///
/// let thread = DedicatedThread::spawn(ThreadConfig::new("cv-ticker"), |stop| {
///     let mut ticks = 0_u32;
///     while !stop.wait_timeout(Duration::from_millis(1)) {
///         ticks += 1;
///     }
///     ticks
/// })?;
/// let ticks = thread.stop_and_join();
/// # let _ = ticks;
/// # Ok::<(), threading::ThreadingError>(())
/// ```
///
/// A token made with [`new`](Self::new) can also be shared by several threads, to stop them all with one call.
#[derive(Clone)]
pub struct StopToken {
    state: Arc<StopState>,
}

impl StopToken {
    /// A new request that has not been made yet.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Arc::new(StopState {
                stopped: AtomicBool::new(false),
                lock: Mutex::new(()),
                wake: Condvar::new(),
            }),
        }
    }

    /// Whether a stop has been requested (by any handle to this token).
    #[must_use]
    pub fn is_stopped(&self) -> bool {
        self.state.stopped.load(Ordering::Acquire)
    }

    /// Requests the stop and wakes every thread that sleeps in [`wait`](Self::wait) or
    /// [`wait_timeout`](Self::wait_timeout). Idempotent, and it never blocks.
    pub fn request_stop(&self) {
        // Setting the flag under the lock closes the gap between a sleeper's check and its sleep.
        let _guard = self
            .state
            .lock
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        self.state.stopped.store(true, Ordering::Release);
        self.state.wake.notify_all();
    }

    /// Sleeps until a stop is requested.
    pub fn wait(&self) {
        let guard = self
            .state
            .lock
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let _guard = self
            .state
            .wake
            .wait_while(guard, |()| !self.is_stopped())
            .unwrap_or_else(PoisonError::into_inner);
    }

    /// Sleeps for at most `timeout`, and wakes up early when a stop is requested. Returns whether a stop has
    /// been requested (so `false` means the time ran out). A thread that paces itself uses this instead of
    /// `thread::sleep`, so that stopping it never has to wait for a nap to end.
    #[must_use]
    pub fn wait_timeout(&self, timeout: Duration) -> bool {
        let guard = self
            .state
            .lock
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let (_guard, _timed_out) = self
            .state
            .wake
            .wait_timeout_while(guard, timeout, |()| !self.is_stopped())
            .unwrap_or_else(PoisonError::into_inner);
        self.is_stopped()
    }
}

impl Default for StopToken {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for StopToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StopToken")
            .field("stopped", &self.is_stopped())
            .finish()
    }
}

/// A thread of its own, with a name, a [role](ThreadRole), a priority and a lifetime the owner controls.
///
/// The closure given to [`spawn`](Self::spawn) runs on the new thread and receives a [`StopToken`]. The thread
/// ends when the closure returns; its return value is collected with [`join`](Self::join).
///
/// # Lifetime
///
/// The handle owns the thread. **Dropping it asks the thread to stop and waits for it to end**, so a thread
/// cannot outlive the object that started it by accident (and it cannot be left running against data that is
/// being torn down). That wait lasts as long as the closure takes to notice the request, so the closure must
/// poll its token. To let a thread run on its own, say so with [`detach`](Self::detach).
///
/// # Panics
///
/// A panic in the closure ends the thread, not the process: [`join`](Self::join) re-raises it and
/// [`join_catch`](Self::join_catch) returns it as a [`JobPanic`]. Dropping the handle discards it.
///
/// ```
/// use std::sync::mpsc;
/// use threading::{DedicatedThread, ThreadConfig, ThreadRole};
///
/// let (sender, receiver) = mpsc::channel();
/// let audio = DedicatedThread::spawn(
///     ThreadConfig::new("cv-audio").with_role(ThreadRole::Audio),
///     move |stop| {
///         ThreadRole::Audio.assert_current();
///         while !stop.is_stopped() {
///             match receiver.recv_timeout(std::time::Duration::from_millis(5)) {
///                 Ok(value) => assert_eq!(value, 7),
///                 Err(_) => {}
///             }
///         }
///         "stopped cleanly"
///     },
/// )?;
/// sender.send(7).unwrap();
/// assert_eq!(audio.stop_and_join(), "stopped cleanly");
/// # Ok::<(), threading::ThreadingError>(())
/// ```
pub struct DedicatedThread<T> {
    name: String,
    role: ThreadRole,
    stop: StopToken,
    /// `None` once the thread has been joined or detached.
    join: Option<JoinHandle<Result<T, PanicPayload>>>,
}

impl<T: Send + 'static> DedicatedThread<T> {
    /// Starts the thread. It names itself, sets its role and asks for its priority before it calls `body`.
    ///
    /// # Errors
    ///
    /// * [`ThreadingError::InvalidConfig`] when [`ThreadConfig::validate`] rejects the config.
    /// * [`ThreadingError::ThreadSpawn`] when the OS refuses to start the thread.
    pub fn spawn<F>(config: ThreadConfig, body: F) -> ThreadingResult<Self>
    where
        F: FnOnce(StopToken) -> T + Send + 'static,
    {
        config.validate()?;
        let stop = StopToken::new();
        let token = stop.clone();
        let (role, priority) = (config.role, config.priority);

        let mut builder = thread::Builder::new().name(config.name.clone());
        if let Some(bytes) = config.stack_size {
            builder = builder.stack_size(bytes);
        }
        let join = builder
            .spawn(move || {
                role::set_current(role);
                if priority != ThreadPriority::Normal {
                    // A request, as for the workers of a pool.
                    let _ = platform::thread::set_priority(priority);
                }
                panic::catch_unwind(AssertUnwindSafe(move || body(token)))
            })
            .map_err(|source| ThreadingError::ThreadSpawn {
                name: config.name.clone(),
                source,
            })?;

        Ok(Self {
            name: config.name,
            role,
            stop,
            join: Some(join),
        })
    }

    /// The name the thread was started with.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The role of the thread.
    #[must_use]
    pub fn role(&self) -> ThreadRole {
        self.role
    }

    /// A handle to the thread's stop request, for code that wants to stop it from elsewhere (a shutdown
    /// path that does not own the `DedicatedThread`) or to see whether it was asked to.
    #[must_use]
    pub fn stop_token(&self) -> StopToken {
        self.stop.clone()
    }

    /// Asks the thread to stop. It does not wait: the thread ends when its closure notices and returns.
    pub fn request_stop(&self) {
        self.stop.request_stop();
    }

    /// Whether a stop has been requested.
    #[must_use]
    pub fn is_stop_requested(&self) -> bool {
        self.stop.is_stopped()
    }

    /// Whether the thread has ended (its closure returned or panicked). A finished thread can be joined
    /// without waiting.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.join.as_ref().is_none_or(JoinHandle::is_finished)
    }

    /// Waits for the thread to end and returns what its closure returned.
    ///
    /// This does not ask the thread to stop; see [`stop_and_join`](Self::stop_and_join).
    ///
    /// # Panics
    ///
    /// Re-raises the panic of the closure with its original payload, and panics when called on the thread
    /// itself (a thread cannot wait for its own end).
    pub fn join(self) -> T {
        match self.join_catch() {
            Ok(value) => value,
            Err(failure) => panic::resume_unwind(failure.into_payload()),
        }
    }

    /// Like [`join`](Self::join), but a panic of the closure is returned as a [`JobPanic`].
    ///
    /// # Errors
    ///
    /// [`JobPanic`] when the closure panicked.
    ///
    /// # Panics
    ///
    /// When called on the thread itself.
    pub fn join_catch(mut self) -> Result<T, JobPanic> {
        let join = self.join.take().expect("a handle is joined only once");
        assert!(
            join.thread().id() != thread::current().id(),
            "a thread cannot wait for its own end"
        );
        match join.join() {
            Ok(Ok(value)) => Ok(value),
            // The closure panicked (caught inside the thread), or something outside it did.
            Ok(Err(payload)) | Err(payload) => Err(JobPanic::new(payload)),
        }
    }

    /// Asks the thread to stop, waits for it to end and returns what its closure returned.
    ///
    /// # Panics
    ///
    /// As [`join`](Self::join).
    pub fn stop_and_join(self) -> T {
        self.request_stop();
        self.join()
    }

    /// Like [`stop_and_join`](Self::stop_and_join), but a panic of the closure is returned as a
    /// [`JobPanic`].
    ///
    /// # Errors
    ///
    /// [`JobPanic`] when the closure panicked.
    pub fn stop_and_join_catch(self) -> Result<T, JobPanic> {
        self.request_stop();
        self.join_catch()
    }

    /// Lets the thread run on its own: the handle is consumed and dropping it no longer stops anything. The
    /// thread ends when its closure returns, and the process may end before that. The returned token is the
    /// only way left to ask it to stop.
    pub fn detach(mut self) -> StopToken {
        drop(self.join.take());
        self.stop.clone()
    }
}

impl<T> Drop for DedicatedThread<T> {
    fn drop(&mut self) {
        let Some(join) = self.join.take() else { return };
        self.stop.request_stop();
        if join.thread().id() == thread::current().id() {
            // The thread holds its own handle (it was sent to it): waiting would never end.
            return;
        }
        // The result is not wanted. Dropping a payload can panic; that must not escape a destructor.
        if let Ok(Err(payload)) = join.join() {
            discard_payload(payload);
        }
    }
}

impl<T> fmt::Debug for DedicatedThread<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DedicatedThread")
            .field("name", &self.name)
            .field("role", &self.role)
            .field("stop_requested", &self.stop.is_stopped())
            .field(
                "finished",
                &self.join.as_ref().is_none_or(JoinHandle::is_finished),
            )
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_config_validates_name_and_stack_size() {
        assert!(ThreadConfig::new("cv-x").validate().is_ok());
        assert!(ThreadConfig::new("").validate().is_err());
        assert!(ThreadConfig::new("a\0b").validate().is_err());
        assert!(
            ThreadConfig::new("x")
                .with_stack_size(0)
                .validate()
                .is_err()
        );
    }

    #[test]
    fn the_config_defaults_and_setters() {
        let config = ThreadConfig::new("x");
        assert_eq!(config.role(), ThreadRole::Unknown);
        assert_eq!(config.priority(), ThreadPriority::Normal);
        assert_eq!(config.stack_size(), None);
        let config = config
            .with_role(ThreadRole::Render)
            .with_priority(ThreadPriority::High)
            .with_stack_size(1 << 20);
        assert_eq!(config.role(), ThreadRole::Render);
        assert_eq!(config.priority(), ThreadPriority::High);
        assert_eq!(config.stack_size(), Some(1 << 20));
    }

    #[test]
    fn a_token_starts_unstopped_and_every_clone_sees_the_request() {
        let token = StopToken::new();
        let copy = token.clone();
        assert!(!token.is_stopped() && !copy.is_stopped());
        copy.request_stop();
        copy.request_stop();
        assert!(token.is_stopped());
        assert!(
            token.wait_timeout(Duration::from_secs(60)),
            "returns at once"
        );
        token.wait();
    }

    #[test]
    fn a_wait_that_times_out_says_so() {
        let token = StopToken::new();
        assert!(!token.wait_timeout(Duration::from_millis(1)));
    }
}
