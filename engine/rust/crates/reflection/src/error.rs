//! The error type of the reflective operations.

use std::fmt;

use ccore::CoreError;

use crate::reflect::ReflectKind;

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
    /// Two different types claim the same name: the path or an alias of one is the path or an alias of the
    /// other.
    NameConflict {
        /// The contested name.
        name: &'static str,
        /// The path of the type that already has it.
        existing: &'static str,
        /// The path of the type that was refused.
        new: &'static str,
    },
    /// Two different paths have the same 64-bit hash.
    HashCollision {
        /// The shared hash.
        id: u64,
        /// The path of the type that is already registered.
        existing: &'static str,
        /// The path of the type that was refused.
        new: &'static str,
    },
    /// No type is registered under the given name.
    UnknownType {
        /// The path or alias that was asked for.
        name: String,
    },
    /// The type is registered but has no type data of the kind that was asked for.
    MissingTypeData {
        /// The path of the type.
        path: &'static str,
        /// The name of the type data (`ReflectDefault`, ...).
        data: &'static str,
    },
    /// The text of a path could not be parsed.
    InvalidPath {
        /// The text that was given.
        path: String,
        /// The byte position of the problem in the text.
        position: usize,
        /// What is wrong.
        reason: &'static str,
    },
    /// A path does not fit the value it was applied to.
    PathMismatch {
        /// The text of the path.
        path: String,
        /// The position of the failing segment (0 is the first).
        segment: usize,
        /// What is wrong.
        reason: &'static str,
    },
    /// A value of another kind than the operation needs.
    KindMismatch {
        /// The kind the operation needs.
        expected: ReflectKind,
        /// The kind of the value that was given.
        actual: ReflectKind,
    },
    /// An array (or other fixed-size value) of another length.
    LengthMismatch {
        /// The length the operation needs.
        expected: usize,
        /// The length of the value that was given.
        actual: usize,
    },
    /// The operation is not possible for this type: an opaque value that cannot be cloned, a value that cannot
    /// be built from a reflected one, ...
    Unsupported {
        /// What was attempted.
        operation: &'static str,
        /// The path of the type.
        path: &'static str,
    },
}

impl ReflectError {
    /// A [`ReflectError::TypeMismatch`] for the given type paths.
    pub const fn type_mismatch(expected: &'static str, actual: &'static str) -> Self {
        Self::TypeMismatch { expected, actual }
    }

    /// A [`ReflectError::Unsupported`] for the given operation and type path.
    pub const fn unsupported(operation: &'static str, path: &'static str) -> Self {
        Self::Unsupported { operation, path }
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
            Self::NameConflict {
                name,
                existing,
                new,
            } => write!(
                f,
                "the name `{name}` is already used by `{existing}`; `{new}` cannot use it"
            ),
            Self::HashCollision { id, existing, new } => write!(
                f,
                "the paths `{existing}` and `{new}` have the same hash ({id:#018x})"
            ),
            Self::UnknownType { name } => write!(f, "no type is registered under `{name}`"),
            Self::MissingTypeData { path, data } => {
                write!(f, "`{path}` has no `{data}` registered")
            }
            Self::InvalidPath {
                path,
                position,
                reason,
            } => write!(f, "invalid path `{path}` at byte {position}: {reason}"),
            Self::PathMismatch {
                path,
                segment,
                reason,
            } => write!(
                f,
                "path `{path}` does not fit at segment {segment}: {reason}"
            ),
            Self::KindMismatch { expected, actual } => {
                write!(f, "kind mismatch: expected a {expected}, got a {actual}")
            }
            Self::LengthMismatch { expected, actual } => {
                write!(f, "length mismatch: expected {expected}, got {actual}")
            }
            Self::Unsupported { operation, path } => {
                write!(f, "cannot {operation} a value of type `{path}`")
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
        let conflict = ReflectError::NameConflict {
            name: "a::B",
            existing: "a::B",
            new: "c::D",
        };
        assert_eq!(
            conflict.to_string(),
            "the name `a::B` is already used by `a::B`; `c::D` cannot use it"
        );
        let unknown = ReflectError::UnknownType {
            name: "x::Y".to_string(),
        };
        assert_eq!(unknown.to_string(), "no type is registered under `x::Y`");
        let missing = ReflectError::MissingTypeData {
            path: "x::Y",
            data: "ReflectDefault",
        };
        assert_eq!(
            missing.to_string(),
            "`x::Y` has no `ReflectDefault` registered"
        );
        let invalid = ReflectError::InvalidPath {
            path: "a[".to_string(),
            position: 1,
            reason: "missing `]`",
        };
        assert_eq!(
            invalid.to_string(),
            "invalid path `a[` at byte 1: missing `]`"
        );
        let kinds = ReflectError::KindMismatch {
            expected: ReflectKind::Struct,
            actual: ReflectKind::List,
        };
        assert_eq!(
            kinds.to_string(),
            "kind mismatch: expected a struct, got a list"
        );
        let unsupported = ReflectError::unsupported("clone", "x::Y");
        assert_eq!(
            unsupported.to_string(),
            "cannot clone a value of type `x::Y`"
        );
    }

    #[test]
    fn converts_into_core_error() {
        let err: CoreError = ReflectError::index_out_of_bounds(1, 0).into();
        assert!(matches!(err, CoreError::InvalidArgument(_)));
    }
}
