//! The error type of the reflective operations.

use std::fmt;

use ccore::CoreError;

/// Why a reflective operation failed.
///
/// The enum is `#[non_exhaustive]`: later milestones add variants (paths, dynamic values).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReflectError {
    /// A value of another type than the one required was given.
    TypeMismatch {
        /// The path of the type that was required.
        expected: &'static str,
        /// The path of the type that was given.
        actual: &'static str,
    },
    /// An index lies outside the container.
    IndexOutOfBounds {
        /// The index that was asked for.
        index: usize,
        /// The length of the container at that moment.
        len: usize,
    },
}

impl ReflectError {
    /// A [`ReflectError::TypeMismatch`] for the given type paths.
    pub const fn type_mismatch(expected: &'static str, actual: &'static str) -> Self {
        Self::TypeMismatch { expected, actual }
    }

    /// A [`ReflectError::IndexOutOfBounds`] for the given index and length.
    pub const fn index_out_of_bounds(index: usize, len: usize) -> Self {
        Self::IndexOutOfBounds { index, len }
    }
}

impl fmt::Display for ReflectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TypeMismatch { expected, actual } => {
                write!(f, "type mismatch: expected `{expected}`, got `{actual}`")
            }
            Self::IndexOutOfBounds { index, len } => {
                write!(f, "index {index} is out of bounds (length {len})")
            }
        }
    }
}

impl std::error::Error for ReflectError {}

impl From<ReflectError> for CoreError {
    fn from(err: ReflectError) -> Self {
        CoreError::InvalidArgument(err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn displays_readable_messages() {
        let mismatch = ReflectError::type_mismatch("u32", "String");
        assert_eq!(
            mismatch.to_string(),
            "type mismatch: expected `u32`, got `String`"
        );
        let bounds = ReflectError::index_out_of_bounds(3, 2);
        assert_eq!(bounds.to_string(), "index 3 is out of bounds (length 2)");
    }

    #[test]
    fn converts_into_core_error() {
        let err: CoreError = ReflectError::index_out_of_bounds(1, 0).into();
        assert!(matches!(err, CoreError::InvalidArgument(_)));
    }
}
