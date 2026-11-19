//! How a [`ThreadPool`](crate::ThreadPool) is set up.

use platform::thread::ThreadPriority;

use crate::error::{ThreadingError, ThreadingResult};
use crate::role::ThreadRole;

/// The largest number of workers a pool accepts. A guard against a typo (or an overflowed computation) asking
/// the OS for a hundred thousand threads; no machine has a use for more.
pub const MAX_WORKERS: usize = 1024;

/// The worker thread name prefix of a default [`PoolConfig`]. Workers are named `<prefix>-<index>`, for
/// example `cv-worker-0`.
pub const DEFAULT_NAME_PREFIX: &str = "cv-worker";

/// The worker thread name prefix of [`PoolConfig::io`]: `cv-io-0`, `cv-io-1`.
pub const IO_NAME_PREFIX: &str = "cv-io";

/// How many workers [`PoolConfig::io`] starts. Two is enough for the blocking calls of a game (reading assets,
/// writing saves) to overlap without starting a thread per file; raise it with
/// [`with_workers`](PoolConfig::with_workers) for a tool that reads from slow storage in bulk.
pub const DEFAULT_IO_WORKERS: usize = 2;

/// How many worker threads a pool starts.
#[derive(Copy, Clone, Eq, PartialEq, Debug, Default)]
pub enum WorkerCount {
    /// One worker fewer than the machine has logical cores, but at least one: the thread that creates the pool
    /// (the game loop, the editor's GUI thread) keeps a core for itself. Resolved when the pool is created.
    #[default]
    Auto,
    /// Exactly this many workers. `Exact(0)` starts no thread at all: jobs then run on the thread that waits
    /// for them or calls [`ThreadPool::run_pending`](crate::ThreadPool::run_pending). That mode is
    /// deterministic, which is what tests and single-threaded builds want.
    Exact(usize),
}

impl WorkerCount {
    /// The number of workers this setting means on this machine.
    #[must_use]
    pub fn resolve(self) -> usize {
        match self {
            Self::Auto => auto_workers(logical_cores()),
            Self::Exact(count) => count,
        }
    }
}

/// The number of logical cores, 1 when the OS does not say.
///
/// This is the same call `platform::info::cpu_info()` makes for its `logical_cores`, without the physical-core
/// query `cpu_info()` also runs: `Auto` has no use for that answer, and the query is an OS call that costs an
/// allocation and a parse (on Windows `GetLogicalProcessorInformationEx`, which Miri cannot execute).
fn logical_cores() -> usize {
    std::thread::available_parallelism().map_or(1, std::num::NonZero::get)
}

/// [`WorkerCount::Auto`] for a machine with `logical_cores` cores.
fn auto_workers(logical_cores: usize) -> usize {
    logical_cores.saturating_sub(1).clamp(1, MAX_WORKERS)
}

/// The settings of a [`ThreadPool`](crate::ThreadPool). Built with the chained setters; every setter takes
/// and returns the config by value.
///
/// ```
/// use threading::{PoolConfig, ThreadPriority, WorkerCount};
///
/// let config = PoolConfig::new()
///     .with_workers(WorkerCount::Exact(4))
///     .with_name_prefix("cv-job")
///     .with_thread_priority(ThreadPriority::Normal);
/// assert_eq!(config.workers().resolve(), 4);
/// assert_eq!(config.name_prefix(), "cv-job");
/// ```
#[derive(Clone, Debug)]
pub struct PoolConfig {
    workers: WorkerCount,
    name_prefix: String,
    thread_priority: ThreadPriority,
    role: ThreadRole,
}

impl PoolConfig {
    /// The defaults: [`WorkerCount::Auto`], the prefix [`DEFAULT_NAME_PREFIX`], normal thread priority and the
    /// role [`ThreadRole::Worker`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            workers: WorkerCount::Auto,
            name_prefix: DEFAULT_NAME_PREFIX.to_owned(),
            thread_priority: ThreadPriority::Normal,
            role: ThreadRole::Worker,
        }
    }

    /// The preset of an **IO pool**: [`DEFAULT_IO_WORKERS`] workers named `cv-io-<index>` with the role
    /// [`ThreadRole::Io`].
    ///
    /// Blocking work (reading a file, waiting for a socket) belongs on a pool of its own, so that it cannot
    /// occupy the workers that the frame's computation needs. There is no hidden global IO pool: the runtime
    /// creates this one next to the compute pool and hands out a [`Spawner`](crate::Spawner) of it, which is
    /// what `asset` and `vfs` will take. The pool is an ordinary [`ThreadPool`](crate::ThreadPool), so
    /// priorities, handles, scopes and [`JobHandle::then_on`](crate::JobHandle::then_on) all work on it.
    ///
    /// ```
    /// use threading::{PoolConfig, ThreadPool, ThreadRole};
    ///
    /// let io = ThreadPool::new(PoolConfig::io())?;
    /// assert_eq!(io.worker_count(), 2);
    /// let role = io.spawn(ThreadRole::current).wait();
    /// assert_eq!(role, ThreadRole::Io);
    /// # Ok::<(), threading::ThreadingError>(())
    /// ```
    #[must_use]
    pub fn io() -> Self {
        Self::new()
            .with_workers(WorkerCount::Exact(DEFAULT_IO_WORKERS))
            .with_name_prefix(IO_NAME_PREFIX)
            .with_role(ThreadRole::Io)
    }

    /// Sets how many workers the pool starts.
    #[must_use]
    pub fn with_workers(mut self, workers: WorkerCount) -> Self {
        self.workers = workers;
        self
    }

    /// Sets the prefix of the worker thread names (`<prefix>-<index>`). The OS may shorten the final name
    /// (Linux keeps 15 bytes).
    #[must_use]
    pub fn with_name_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.name_prefix = prefix.into();
        self
    }

    /// Sets the OS priority every worker asks for when it starts.
    ///
    /// This is a request, not a guarantee: when the OS refuses (on Linux, raising a priority needs a
    /// privilege) the worker simply keeps the default priority. [`ThreadPriority::Normal`] asks for nothing.
    #[must_use]
    pub fn with_thread_priority(mut self, priority: ThreadPriority) -> Self {
        self.thread_priority = priority;
        self
    }

    /// Sets the [role](ThreadRole) every worker gets (default [`ThreadRole::Worker`]). It is what
    /// [`ThreadRole::current`] answers on the pool's own threads.
    #[must_use]
    pub fn with_role(mut self, role: ThreadRole) -> Self {
        self.role = role;
        self
    }

    /// The configured worker count (not yet resolved; see [`WorkerCount::resolve`]).
    #[must_use]
    pub fn workers(&self) -> WorkerCount {
        self.workers
    }

    /// The configured worker thread name prefix.
    #[must_use]
    pub fn name_prefix(&self) -> &str {
        &self.name_prefix
    }

    /// The configured worker thread priority.
    #[must_use]
    pub fn thread_priority(&self) -> ThreadPriority {
        self.thread_priority
    }

    /// The role the workers get.
    #[must_use]
    pub fn role(&self) -> ThreadRole {
        self.role
    }

    /// Checks the settings without starting anything.
    ///
    /// # Errors
    ///
    /// [`ThreadingError::InvalidConfig`] when the prefix contains a NUL character (no OS accepts that in a
    /// thread name) or an exact worker count is above [`MAX_WORKERS`].
    pub fn validate(&self) -> ThreadingResult<()> {
        if self.name_prefix.contains('\0') {
            return Err(ThreadingError::InvalidConfig {
                reason: "the thread name prefix contains a NUL character",
            });
        }
        if let WorkerCount::Exact(count) = self.workers
            && count > MAX_WORKERS
        {
            return Err(ThreadingError::InvalidConfig {
                reason: "the worker count is above MAX_WORKERS",
            });
        }
        Ok(())
    }
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_auto_normal_and_named() {
        let config = PoolConfig::default();
        assert_eq!(config.workers(), WorkerCount::Auto);
        assert_eq!(config.name_prefix(), DEFAULT_NAME_PREFIX);
        assert_eq!(config.thread_priority(), ThreadPriority::Normal);
        assert_eq!(config.role(), ThreadRole::Worker);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn the_io_preset_has_two_named_io_workers() {
        let config = PoolConfig::io();
        assert_eq!(config.workers(), WorkerCount::Exact(DEFAULT_IO_WORKERS));
        assert_eq!(config.name_prefix(), IO_NAME_PREFIX);
        assert_eq!(config.role(), ThreadRole::Io);
        assert!(config.validate().is_ok());
        let more = PoolConfig::io().with_workers(WorkerCount::Exact(6));
        assert_eq!(more.role(), ThreadRole::Io, "other setters keep the preset");
    }

    #[test]
    fn setters_replace_the_values() {
        let config = PoolConfig::new()
            .with_workers(WorkerCount::Exact(2))
            .with_name_prefix("x")
            .with_thread_priority(ThreadPriority::Low)
            .with_role(ThreadRole::Custom("physics"));
        assert_eq!(config.role(), ThreadRole::Custom("physics"));
        assert_eq!(config.workers(), WorkerCount::Exact(2));
        assert_eq!(config.name_prefix(), "x");
        assert_eq!(config.thread_priority(), ThreadPriority::Low);
    }

    #[test]
    fn auto_leaves_one_core_free_but_never_goes_below_one() {
        assert_eq!(auto_workers(0), 1);
        assert_eq!(auto_workers(1), 1);
        assert_eq!(auto_workers(2), 1);
        assert_eq!(auto_workers(8), 7);
        assert_eq!(auto_workers(100_000), MAX_WORKERS);
    }

    #[test]
    fn the_logical_core_count_agrees_with_the_platform_crate() {
        // Native only: `platform::info::cpu_info()` also asks the OS for the physical cores, which Miri
        // cannot do on Windows. The point of this test is that `Auto` and `cpu_info()` see the same machine.
        if !cfg!(miri) {
            assert_eq!(logical_cores(), platform::info::cpu_info().logical_cores);
        }
        assert!(logical_cores() >= 1);
    }

    #[test]
    fn auto_resolves_to_a_usable_count_on_this_machine() {
        let count = WorkerCount::Auto.resolve();
        assert!((1..=MAX_WORKERS).contains(&count));
    }

    #[test]
    fn exact_resolves_to_itself_including_zero() {
        assert_eq!(WorkerCount::Exact(0).resolve(), 0);
        assert_eq!(WorkerCount::Exact(5).resolve(), 5);
    }

    #[test]
    fn validation_rejects_a_nul_in_the_prefix() {
        let err = PoolConfig::new()
            .with_name_prefix("a\0b")
            .validate()
            .unwrap_err();
        assert!(matches!(err, ThreadingError::InvalidConfig { .. }), "{err}");
    }

    #[test]
    fn validation_rejects_too_many_workers_and_accepts_the_limit() {
        let too_many = PoolConfig::new().with_workers(WorkerCount::Exact(MAX_WORKERS + 1));
        assert!(matches!(
            too_many.validate(),
            Err(ThreadingError::InvalidConfig { .. })
        ));
        let limit = PoolConfig::new().with_workers(WorkerCount::Exact(MAX_WORKERS));
        assert!(limit.validate().is_ok());
    }
}
