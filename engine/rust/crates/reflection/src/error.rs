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
    }

    #[test]
    fn converts_into_core_error() {
        let err: CoreError = ReflectError::index_out_of_bounds(1, 0).into();
        assert!(matches!(err, CoreError::InvalidArgument(_)));
    }
}
