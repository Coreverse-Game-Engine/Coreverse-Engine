//! Type data: small function tables stored next to a type's description in the registry.
//!
//! A reflected value does not know whether its type is `Default`, `Clone`, `PartialEq`, `Debug` or `Hash`.
//! The registry remembers it: `#[reflect(Default, Clone)]` (or [`Typed::register_type_data`]) stores a
//! [`ReflectDefault`] and a [`ReflectClone`] for the type, and code that only has a `&dyn Reflect` or a type
//! name can still use the trait through them.

use std::any::Any;
use std::fmt;
use std::hash::{Hash, Hasher};

use crate::error::ReflectError;
use crate::reflect::{Reflect, Typed};

/// Anything that can be stored as type data: every `Any + Send + Sync` type.
pub trait TypeData: Any + Send + Sync {}

impl<T: Any + Send + Sync> TypeData for T {}

/// Makes the type data `Self` for the type `T`. `#[reflect(Name)]` on a type calls
/// `<Name as FromType<ThatType>>::from_type()`, so a type data of your own only has to implement this trait.
pub trait FromType<T> {
    /// The type data for `T`.
    fn from_type() -> Self;
}

fn mismatch<T: Typed>(value: &dyn Reflect) -> ReflectError {
    ReflectError::type_mismatch(T::static_type_info().path(), value.type_info().path())
}

/// Makes a default value of a type that is `Default`.
#[derive(Clone, Copy)]
pub struct ReflectDefault {
    create: fn() -> Box<dyn Reflect>,
}

impl ReflectDefault {
    /// A new default value of the type.
    pub fn create(&self) -> Box<dyn Reflect> {
        (self.create)()
    }
}

impl<T: Typed + Default> FromType<T> for ReflectDefault {
    fn from_type() -> Self {
        Self {
            create: || Box::new(T::default()) as Box<dyn Reflect>,
        }
    }
}

impl fmt::Debug for ReflectDefault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReflectDefault").finish_non_exhaustive()
    }
}

/// The function a [`ReflectClone`] calls.
type CloneFn = fn(&dyn Reflect) -> Result<Box<dyn Reflect>, ReflectError>;

/// Clones a value of a type that is `Clone`.
#[derive(Clone, Copy)]
pub struct ReflectClone {
    clone: CloneFn,
}

impl ReflectClone {
    /// Clones `value`.
    ///
    /// # Errors
    ///
    /// [`ReflectError::TypeMismatch`] if `value` is not of the type this type data belongs to.
    pub fn clone_value(&self, value: &dyn Reflect) -> Result<Box<dyn Reflect>, ReflectError> {
        (self.clone)(value)
    }
}

impl<T: Typed + Clone> FromType<T> for ReflectClone {
    fn from_type() -> Self {
        Self {
            clone: |value: &dyn Reflect| match value.downcast_ref::<T>() {
                Some(inner) => Ok(Box::new(inner.clone()) as Box<dyn Reflect>),
                None => Err(mismatch::<T>(value)),
            },
        }
    }
}

impl fmt::Debug for ReflectClone {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReflectClone").finish_non_exhaustive()
    }
}

/// Compares two values of a type that is `PartialEq`.
#[derive(Clone, Copy)]
pub struct ReflectPartialEq {
    eq: fn(&dyn Reflect, &dyn Reflect) -> Result<bool, ReflectError>,
}

impl ReflectPartialEq {
    /// Whether `a` and `b` are equal.
    ///
    /// # Errors
    ///
    /// [`ReflectError::TypeMismatch`] if one of the values is not of the type this type data belongs to.
    pub fn eq(&self, a: &dyn Reflect, b: &dyn Reflect) -> Result<bool, ReflectError> {
        (self.eq)(a, b)
    }
}

impl<T: Typed + PartialEq> FromType<T> for ReflectPartialEq {
    fn from_type() -> Self {
        Self {
            eq: |a: &dyn Reflect, b: &dyn Reflect| -> Result<bool, ReflectError> {
                let left = a.downcast_ref::<T>().ok_or_else(|| mismatch::<T>(a))?;
                let right = b.downcast_ref::<T>().ok_or_else(|| mismatch::<T>(b))?;
                Ok(left == right)
            },
        }
    }
}

impl fmt::Debug for ReflectPartialEq {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReflectPartialEq").finish_non_exhaustive()
    }
}

/// Formats a value of a type that is `Debug`.
#[derive(Clone, Copy)]
pub struct ReflectDebug {
    fmt: fn(&dyn Reflect, &mut fmt::Formatter<'_>) -> fmt::Result,
}

impl ReflectDebug {
    /// A wrapper that prints `value` with the `Debug` implementation of its type. A value of another type
    /// prints as `<type mismatch>`.
    pub fn debug<'a>(&'a self, value: &'a dyn Reflect) -> DebugValue<'a> {
        DebugValue { data: self, value }
    }
}

impl<T: Typed + fmt::Debug> FromType<T> for ReflectDebug {
    fn from_type() -> Self {
        Self {
            fmt: |value: &dyn Reflect, f: &mut fmt::Formatter<'_>| match value.downcast_ref::<T>() {
                Some(inner) => fmt::Debug::fmt(inner, f),
                None => f.write_str("<type mismatch>"),
            },
        }
    }
}

impl fmt::Debug for ReflectDebug {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReflectDebug").finish_non_exhaustive()
    }
}

/// A reflected value together with the [`ReflectDebug`] that prints it; made by [`ReflectDebug::debug`].
pub struct DebugValue<'a> {
    data: &'a ReflectDebug,
    value: &'a dyn Reflect,
}

impl fmt::Debug for DebugValue<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        (self.data.fmt)(self.value, f)
    }
}

/// Hashes a value of a type that is `Hash`.
#[derive(Clone, Copy)]
pub struct ReflectHash {
    hash: fn(&dyn Reflect, &mut dyn Hasher) -> Result<(), ReflectError>,
}

impl ReflectHash {
    /// Feeds `value` to `state` the way the `Hash` implementation of its type does.
    ///
    /// # Errors
    ///
    /// [`ReflectError::TypeMismatch`] if `value` is not of the type this type data belongs to.
    pub fn hash_value(
        &self,
        value: &dyn Reflect,
        state: &mut dyn Hasher,
    ) -> Result<(), ReflectError> {
        (self.hash)(value, state)
    }
}

impl<T: Typed + Hash> FromType<T> for ReflectHash {
    fn from_type() -> Self {
        Self {
            hash: |value: &dyn Reflect, mut state: &mut dyn Hasher| -> Result<(), ReflectError> {
                match value.downcast_ref::<T>() {
                    Some(inner) => {
                        inner.hash(&mut state);
                        Ok(())
                    }
                    None => Err(mismatch::<T>(value)),
                }
            },
        }
    }
}

impl fmt::Debug for ReflectHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReflectHash").finish_non_exhaustive()
    }
}

/// The function a [`ReflectFromReflect`] calls.
type FromReflectFn = fn(&dyn Reflect) -> Option<Box<dyn Reflect>>;

/// Builds a value of a type from a reflected value of the same shape ([`Typed::from_reflect`]), for code that
/// only knows the type by name. Every registration has one; for a type that cannot be built from a reflected
/// value (the default of `Typed::from_reflect`) it always returns `None`.
#[derive(Clone, Copy)]
pub struct ReflectFromReflect {
    from_reflect: FromReflectFn,
}

impl ReflectFromReflect {
    /// A new value of the type built from `value`, or `None` if it cannot be built.
    pub fn from_reflect(&self, value: &dyn Reflect) -> Option<Box<dyn Reflect>> {
        (self.from_reflect)(value)
    }
}

impl<T: Typed> FromType<T> for ReflectFromReflect {
    fn from_type() -> Self {
        Self {
            from_reflect: |value: &dyn Reflect| {
                T::from_reflect(value).map(|built| Box::new(built) as Box<dyn Reflect>)
            },
        }
    }
}

impl fmt::Debug for ReflectFromReflect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReflectFromReflect").finish_non_exhaustive()
    }
}
