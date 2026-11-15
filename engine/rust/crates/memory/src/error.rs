//! The error type of the `memory` crate.

use core::fmt;

/// Everything that can go wrong in the helpers and allocators of this crate.
///
/// The enum is `#[non_exhaustive]`: later milestones add variants (for example for exhausted arenas), so a `match`
/// outside this crate needs a wildcard arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum MemoryError {
    /// An alignment was not a power of two (zero is not one either).
    InvalidAlignment {
        /// The rejected alignment.
        align: usize,
    },

    /// A size computation overflowed `usize`, or the result is larger than `isize::MAX`, the biggest size Rust
    /// allows for a single allocation.
    SizeOverflow,

    /// An arena has not enough room left for a request.
    OutOfMemory {
        /// The size of the request in bytes (without alignment padding).
        requested: usize,
        /// The bytes the arena had left when the request came, before any alignment padding. The padding a request
        /// needs can make it fail even when `available >= requested`.
        available: usize,
    },

    /// The system allocator could not provide a block.
    AllocationFailed {
        /// The size of the block in bytes.
        size: usize,
    },

    /// A [`Marker`](crate::Marker) lies beyond the arena's current position: it was taken after a state the arena
    /// has been reset or rewound past.
    InvalidMarker,
}

impl fmt::Display for MemoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidAlignment { align } => {
                write!(f, "alignment {align} is not a power of two")
            }
            Self::SizeOverflow => {
                write!(f, "size computation overflowed or exceeds isize::MAX")
            }
            Self::OutOfMemory {
                requested,
                available,
            } => {
                write!(
                    f,
                    "out of memory: {requested} bytes requested, {available} bytes left"
                )
            }
            Self::AllocationFailed { size } => {
                write!(f, "the system allocator could not provide {size} bytes")
            }
            Self::InvalidMarker => {
                write!(f, "marker lies beyond the current allocation position")
            }
        }
    }
}

impl std::error::Error for MemoryError {}

/// Shorthand for `Result<T, MemoryError>`.
pub type MemoryResult<T> = Result<T, MemoryError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_are_readable() {
        assert_eq!(
            MemoryError::InvalidAlignment { align: 3 }.to_string(),
            "alignment 3 is not a power of two"
        );
        assert_eq!(
            MemoryError::SizeOverflow.to_string(),
            "size computation overflowed or exceeds isize::MAX"
        );
    }

    #[test]
    fn arena_messages_are_readable() {
        let out_of_memory = MemoryError::OutOfMemory {
            requested: 64,
            available: 16,
        };
        assert_eq!(
            out_of_memory.to_string(),
            "out of memory: 64 bytes requested, 16 bytes left"
        );
        assert_eq!(
            MemoryError::AllocationFailed { size: 4096 }.to_string(),
            "the system allocator could not provide 4096 bytes"
        );
        assert_eq!(
            MemoryError::InvalidMarker.to_string(),
            "marker lies beyond the current allocation position"
        );
    }

    #[test]
    fn is_a_std_error_that_can_cross_threads() {
        fn assert_error<E: std::error::Error + Send + Sync + 'static>() {}
        assert_error::<MemoryError>();
    }

    #[test]
    fn errors_compare_by_value() {
        assert_eq!(
            MemoryError::InvalidAlignment { align: 0 },
            MemoryError::InvalidAlignment { align: 0 }
        );
        assert_ne!(
            MemoryError::InvalidAlignment { align: 0 },
            MemoryError::SizeOverflow
        );
    }
}
