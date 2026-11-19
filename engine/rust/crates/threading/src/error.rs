//! The error type of this crate.

use ccore::CoreError;

use crate::role::ThreadRole;

/// Everything that can go wrong when a thread pool is configured or started.
///
/// Running jobs never fails with this type: a job that panics reports the panic through its
/// [`JobHandle`](crate::JobHandle) (see [`JobPanic`](crate::JobPanic)).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ThreadingError {
    /// A [`PoolConfig`](crate::PoolConfig) cannot be used as it is.
    #[error("invalid thread pool configuration: {reason}")]
    InvalidConfig {
        /// What is wrong, as a short sentence fragment.
        reason: &'static str,
    },

    /// The operating system refused to start a worker thread. The workers started before it have been
    /// stopped again, so no half-built pool is left behind.
    #[error("could not start worker thread {index} (\"{name}\"): {source}")]
    Spawn {
        /// Zero-based index of the worker that failed.
        index: usize,
        /// The name the worker would have had.
        name: String,
        /// The error the OS reported.
        #[source]
        source: std::io::Error,
    },

    /// The pool is shutting down (or has shut down) and does not take this job: from the moment the shutdown
    /// begins, a [`Spawner`](crate::Spawner) only accepts jobs from threads that are running one of the pool's
    /// jobs. Only a spawner can run into this: a [`ThreadPool`](crate::ThreadPool) cannot be used once it is
    /// being dropped.
    #[error("the thread pool has shut down")]
    PoolClosed,

    /// The operating system refused to start a [dedicated thread](crate::DedicatedThread).
    #[error("could not start thread \"{name}\": {source}")]
    ThreadSpawn {
        /// The name the thread would have had.
        name: String,
        /// The error the OS reported.
        #[source]
        source: std::io::Error,
    },

    /// Code that is restricted to one kind of thread ran on another; see [`ThreadRole::require`].
    #[error("this must run on the {expected} thread, but the calling thread is {actual}")]
    WrongThread {
        /// The role the code needs.
        expected: ThreadRole,
        /// The role the calling thread has.
        actual: ThreadRole,
    },

    /// A dependency would make a [`TaskGraph`](crate::TaskGraph) cyclic, so its tasks could never all run.
    /// The graph is left as it was.
    #[error("the dependency would create a cycle: {}", .path.join(" -> "))]
    DependencyCycle {
        /// The names of the tasks on the cycle, in the order they would have to run (`a -> b` means `a` runs
        /// before `b`). The first and the last name are the same task.
        path: Vec<String>,
    },

    /// A [`TaskId`](crate::TaskId) does not belong to the graph it was used with (it was made by another
    /// graph).
    #[error("the task id does not belong to this graph")]
    UnknownTask,
}

/// Shorthand for `Result<T, ThreadingError>`.
pub type ThreadingResult<T> = Result<T, ThreadingError>;

impl From<ThreadingError> for CoreError {
    fn from(err: ThreadingError) -> Self {
        let message = err.to_string();
        match err {
            ThreadingError::InvalidConfig { .. } => CoreError::InvalidArgument(message),
            ThreadingError::Spawn { .. } | ThreadingError::ThreadSpawn { .. } => {
                CoreError::Io(message)
            }
            ThreadingError::WrongThread { .. } => CoreError::InvalidState(message),
            ThreadingError::PoolClosed => CoreError::InvalidState(message),
            ThreadingError::DependencyCycle { .. } | ThreadingError::UnknownTask => {
                CoreError::InvalidArgument(message)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_config_names_the_reason() {
        let err = ThreadingError::InvalidConfig {
            reason: "too many workers",
        };
        assert_eq!(
            err.to_string(),
            "invalid thread pool configuration: too many workers"
        );
    }

    #[test]
    fn spawn_error_names_the_worker_and_keeps_the_source() {
        let err = ThreadingError::Spawn {
            index: 3,
            name: "cv-worker-3".to_owned(),
            source: std::io::Error::other("no threads left"),
        };
        let text = err.to_string();
        assert!(text.contains("worker thread 3"), "{text}");
        assert!(text.contains("cv-worker-3"), "{text}");
        assert!(text.contains("no threads left"), "{text}");
        assert!(std::error::Error::source(&err).is_some());
    }

    #[test]
    fn converts_into_core_error() {
        let invalid: CoreError = ThreadingError::InvalidConfig { reason: "x" }.into();
        assert!(matches!(invalid, CoreError::InvalidArgument(_)));
        let spawn: CoreError = ThreadingError::Spawn {
            index: 0,
            name: String::new(),
            source: std::io::Error::other("x"),
        }
        .into();
        assert!(matches!(spawn, CoreError::Io(_)));
        let closed: CoreError = ThreadingError::PoolClosed.into();
        assert!(matches!(closed, CoreError::InvalidState(_)));
        let cycle: CoreError = ThreadingError::DependencyCycle {
            path: vec!["a".to_owned(), "a".to_owned()],
        }
        .into();
        assert!(matches!(cycle, CoreError::InvalidArgument(_)));
        let unknown: CoreError = ThreadingError::UnknownTask.into();
        assert!(matches!(unknown, CoreError::InvalidArgument(_)));
        let wrong: CoreError = ThreadingError::WrongThread {
            expected: ThreadRole::Main,
            actual: ThreadRole::Worker,
        }
        .into();
        assert!(matches!(wrong, CoreError::InvalidState(_)));
        let spawn: CoreError = ThreadingError::ThreadSpawn {
            name: "cv-x".to_owned(),
            source: std::io::Error::other("no threads left"),
        }
        .into();
        assert!(matches!(spawn, CoreError::Io(_)));
    }

    #[test]
    fn the_thread_errors_print_the_names_and_roles() {
        let wrong = ThreadingError::WrongThread {
            expected: ThreadRole::Main,
            actual: ThreadRole::Io,
        };
        assert_eq!(
            wrong.to_string(),
            "this must run on the main thread, but the calling thread is io"
        );
        let spawn = ThreadingError::ThreadSpawn {
            name: "cv-render".to_owned(),
            source: std::io::Error::other("no threads left"),
        };
        let text = spawn.to_string();
        assert!(
            text.contains("cv-render") && text.contains("no threads left"),
            "{text}"
        );
    }

    #[test]
    fn a_cycle_error_prints_the_path() {
        let err = ThreadingError::DependencyCycle {
            path: vec!["load".to_owned(), "parse".to_owned(), "load".to_owned()],
        };
        assert_eq!(
            err.to_string(),
            "the dependency would create a cycle: load -> parse -> load"
        );
    }
}
