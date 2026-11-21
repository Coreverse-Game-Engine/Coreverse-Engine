//! Thread roles: which job the calling thread has in the engine (main thread, worker, IO worker, render
//! thread, ...), and assertions about it.
//!
//! A role is a label on a *thread*, kept in a thread-local. It is set
//!
//! * by the pools, on their own threads: [`ThreadRole::Worker`] for the workers of a default pool,
//!   [`ThreadRole::Io`] for those of [`PoolConfig::io`](crate::PoolConfig::io), or whatever
//!   [`PoolConfig::with_role`](crate::PoolConfig::with_role) says;
//! * by [`DedicatedThread`](crate::DedicatedThread), from its [`ThreadConfig`](crate::ThreadConfig);
//! * by the runtime, on the threads it did not start itself. The main thread is the case that matters: the
//!   standard library cannot tell which thread is "the main thread" in a way an engine can rely on (the editor
//!   runs its GUI on one thread, a test harness runs every test on its own), so the code that owns the loop
//!   says so with [`ThreadRole::Main`]`.enter()`.
//!
//! There is no process-wide "the main thread" variable and nothing here is a global: a role belongs to the thread
//! that entered it, and a guard restores the previous role when it is dropped. That keeps tests independent (each
//! test thread may be a main thread in its own scope).
//!
//! A thread that waits for a job and runs other jobs while it waits ([`JobHandle`](crate::JobHandle)) keeps its own
//! role: the role is the thread's, not the job's. So `assert_current(Worker)` in a job can fail when the job was
//! run by a helping thread, and the same is true for a pool without workers (all of its jobs run on the threads
//! that wait for them).

use std::cell::Cell;
use std::fmt;
use std::marker::PhantomData;
use std::thread;

use crate::error::{ThreadingError, ThreadingResult};

thread_local! {
    static CURRENT: Cell<ThreadRole> = const { Cell::new(ThreadRole::Unknown) };
}

/// What a thread is for. A thread gets its role from its pool, from [`DedicatedThread`](crate::DedicatedThread) or from [`enter`](Self::enter).
///
/// `#[non_exhaustive]`: more roles may be added. [`Custom`](Self::Custom) covers the roles of games and tools
/// built on the engine without a change here.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Default)]
#[non_exhaustive]
pub enum ThreadRole {
    /// A thread nobody has labelled. Every thread starts as this.
    #[default]
    Unknown,
    /// The main thread: the one that runs the game loop (or the GUI thread of the editor), owns windows and the
    /// thread pool, and is the only one allowed to call some APIs.
    Main,
    /// A worker of a compute pool (the default of [`PoolConfig`](crate::PoolConfig)).
    Worker,
    /// A worker of an IO pool, where blocking calls (file reads, sockets) are allowed to take their time.
    Io,
    /// The render thread.
    Render,
    /// The audio thread, which must not block.
    Audio,
    /// A role of the application's own; the string is its name. Two custom roles are the same when their names
    /// are equal.
    Custom(&'static str),
}

impl ThreadRole {
    /// The role of the calling thread ([`Unknown`](Self::Unknown) if nobody has set one).
    #[must_use]
    pub fn current() -> Self {
        CURRENT.with(Cell::get)
    }

    /// Whether the calling thread has this role.
    #[must_use]
    pub fn is_current(self) -> bool {
        Self::current() == self
    }

    /// Gives the calling thread this role until the returned guard is dropped, which restores the role the
    /// thread had before (guards nest).
    ///
    /// This is how the runtime marks the main thread, once, at the top of its `main`:
    ///
    /// ```
    /// use threading::{ThreadRole, is_main_thread};
    ///
    /// assert!(!is_main_thread());
    /// {
    ///     let _main = ThreadRole::Main.enter();
    ///     assert!(is_main_thread());
    /// }
    /// assert!(!is_main_thread());
    /// ```
    ///
    /// The guard cannot be sent to another thread: the role it sets is the calling thread's.
    pub fn enter(self) -> RoleGuard {
        let previous = CURRENT.with(|current| current.replace(self));
        RoleGuard {
            previous,
            _not_send: PhantomData,
        }
    }

    /// Checks that the calling thread has this role.
    ///
    /// # Errors
    ///
    /// [`ThreadingError::WrongThread`] with the expected and the actual role.
    pub fn require(self) -> ThreadingResult<()> {
        let actual = Self::current();
        if actual == self {
            Ok(())
        } else {
            Err(ThreadingError::WrongThread {
                expected: self,
                actual,
            })
        }
    }

    /// Panics unless the calling thread has this role. For functions that may only be called from one kind of
    /// thread:
    ///
    /// ```
    /// use threading::ThreadRole;
    ///
    /// fn create_window() {
    ///     ThreadRole::Main.assert_current();
    ///     // ...
    /// }
    /// # let _main = ThreadRole::Main.enter();
    /// # create_window();
    /// ```
    ///
    /// # Panics
    ///
    /// When the role of the calling thread is another one. The message names both roles, the thread's name and,
    /// through `#[track_caller]`, the call site.
    #[track_caller]
    pub fn assert_current(self) {
        let actual = Self::current();
        assert!(
            actual == self,
            "this code must run on the {self} thread, but it is running on {}",
            describe_current(actual)
        );
    }

    /// Like [`assert_current`](Self::assert_current), but only in builds with debug assertions; in release
    /// builds it does nothing and costs nothing.
    #[track_caller]
    pub fn debug_assert_current(self) {
        if cfg!(debug_assertions) {
            self.assert_current();
        }
    }

    /// Panics if the calling thread has this role. For code that must stay away from a thread, such as a call
    /// that blocks on IO, which the main thread, the render thread and the audio thread must not make.
    ///
    /// # Panics
    ///
    /// When the calling thread has this role.
    #[track_caller]
    pub fn assert_not_current(self) {
        let actual = Self::current();
        assert!(
            actual != self,
            "this code must not run on the {self} thread, but it is running on {}",
            describe_current(actual)
        );
    }
}

/// `the <role> thread (named "<name>")`, for messages.
fn describe_current(role: ThreadRole) -> String {
    match thread::current().name() {
        Some(name) => format!("a {role} thread (named \"{name}\")"),
        None => format!("a {role} thread (unnamed)"),
    }
}

impl fmt::Display for ThreadRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unknown => f.write_str("unknown"),
            Self::Main => f.write_str("main"),
            Self::Worker => f.write_str("worker"),
            Self::Io => f.write_str("io"),
            Self::Render => f.write_str("render"),
            Self::Audio => f.write_str("audio"),
            Self::Custom(name) => f.write_str(name),
        }
    }
}

/// Keeps the role a thread entered with [`ThreadRole::enter`] and puts the previous role back when it is
/// dropped. Not `Send`: it belongs to the thread that made it.
#[must_use = "the role lasts only as long as the guard"]
pub struct RoleGuard {
    previous: ThreadRole,
    /// Raw pointers are neither `Send` nor `Sync`.
    _not_send: PhantomData<*const ()>,
}

impl Drop for RoleGuard {
    fn drop(&mut self) {
        // `try_with`: this can run while the thread is being torn down, when the slot is already gone.
        let _ = CURRENT.try_with(|current| current.set(self.previous));
    }
}

impl fmt::Debug for RoleGuard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RoleGuard")
            .field("restores", &self.previous)
            .finish()
    }
}

/// Whether the calling thread has been marked as the main thread (see [`ThreadRole::Main`]).
#[must_use]
pub fn is_main_thread() -> bool {
    ThreadRole::Main.is_current()
}

/// Panics unless the calling thread is the main thread: `ThreadRole::Main.assert_current()`.
///
/// # Panics
///
/// When the calling thread has not been marked as the main thread.
#[track_caller]
pub fn assert_main_thread() {
    ThreadRole::Main.assert_current();
}

/// Marks the calling thread for good (until it ends), without a guard. For the threads this crate starts.
pub(crate) fn set_current(role: ThreadRole) {
    CURRENT.with(|current| current.set(role));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_thread_is_unknown() {
        std::thread::spawn(|| {
            assert_eq!(ThreadRole::current(), ThreadRole::Unknown);
            assert!(!is_main_thread());
        })
        .join()
        .unwrap();
    }

    #[test]
    fn entering_a_role_lasts_until_the_guard_drops_and_guards_nest() {
        std::thread::spawn(|| {
            let outer = ThreadRole::Main.enter();
            assert!(ThreadRole::Main.is_current());
            {
                let _inner = ThreadRole::Render.enter();
                assert_eq!(ThreadRole::current(), ThreadRole::Render);
                assert!(!is_main_thread());
            }
            assert!(is_main_thread(), "the inner guard restored the outer role");
            drop(outer);
            assert_eq!(ThreadRole::current(), ThreadRole::Unknown);
        })
        .join()
        .unwrap();
    }

    #[test]
    fn a_role_belongs_to_its_thread() {
        let _main = ThreadRole::Main.enter();
        let other = std::thread::spawn(ThreadRole::current).join().unwrap();
        assert_eq!(other, ThreadRole::Unknown);
        assert!(is_main_thread());
    }

    #[test]
    fn require_returns_both_roles_on_a_mismatch() {
        let _audio = ThreadRole::Audio.enter();
        assert!(ThreadRole::Audio.require().is_ok());
        match ThreadRole::Main.require() {
            Err(ThreadingError::WrongThread { expected, actual }) => {
                assert_eq!(expected, ThreadRole::Main);
                assert_eq!(actual, ThreadRole::Audio);
            }
            other => panic!("unexpected result: {other:?}"),
        }
    }

    #[test]
    fn custom_roles_compare_by_name() {
        let _guard = ThreadRole::Custom("physics").enter();
        assert!(ThreadRole::Custom("physics").is_current());
        assert!(!ThreadRole::Custom("navmesh").is_current());
        assert_eq!(ThreadRole::Custom("physics").to_string(), "physics");
    }

    #[test]
    fn the_assertions_panic_with_a_message_that_names_the_roles() {
        let _io = ThreadRole::Io.enter();
        let failure = std::panic::catch_unwind(|| ThreadRole::Main.assert_current()).unwrap_err();
        let text = failure
            .downcast_ref::<String>()
            .expect("a formatted message");
        assert!(text.contains("main thread"), "{text}");
        assert!(text.contains("io thread"), "{text}");

        ThreadRole::Io.assert_current();
        ThreadRole::Io.debug_assert_current();
        ThreadRole::Main.assert_not_current();
        let failure = std::panic::catch_unwind(|| ThreadRole::Io.assert_not_current()).unwrap_err();
        let text = failure
            .downcast_ref::<String>()
            .expect("a formatted message");
        assert!(text.contains("must not run on the io thread"), "{text}");
    }

    #[test]
    fn assert_main_thread_follows_the_marker() {
        std::thread::spawn(|| {
            assert!(std::panic::catch_unwind(assert_main_thread).is_err());
            let _main = ThreadRole::Main.enter();
            assert_main_thread();
        })
        .join()
        .unwrap();
    }

    #[test]
    fn the_display_names_are_stable() {
        let names: Vec<String> = [
            ThreadRole::Unknown,
            ThreadRole::Main,
            ThreadRole::Worker,
            ThreadRole::Io,
            ThreadRole::Render,
            ThreadRole::Audio,
        ]
        .iter()
        .map(ToString::to_string)
        .collect();
        assert_eq!(
            names,
            ["unknown", "main", "worker", "io", "render", "audio"]
        );
    }
}
