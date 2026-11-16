//! The error type of this crate.

use ccore::CoreError;

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
}

/// Shorthand for `Result<T, ThreadingError>`.
pub type ThreadingResult<T> = Result<T, ThreadingError>;

impl From<ThreadingError> for CoreError {
    fn from(err: ThreadingError) -> Self {
        let message = err.to_string();
        match err {
            ThreadingError::InvalidConfig { .. } => CoreError::InvalidArgument(message),
            ThreadingError::Spawn { .. } => CoreError::Io(message),
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
    }
}
