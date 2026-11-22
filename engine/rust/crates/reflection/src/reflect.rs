//! The reflection traits: [`Reflect`], [`Typed`] and one trait per kind of type.

use std::any::Any;
use std::fmt;

use crate::error::ReflectError;
use crate::info::{TypeInfo, VariantKind};
use crate::registry::TypeRegistration;

/// A boxed iterator over borrowed reflected items; what the container traits return from `iter`.
pub type ReflectIter<'a, I> = Box<dyn Iterator<Item = I> + 'a>;

/// How reflection sees a type. One kind per type, fixed by its [`TypeInfo`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ReflectKind {
    /// A struct with named fields: see [`Struct`].
    Struct,
    /// A tuple struct: see [`TupleStruct`].
    TupleStruct,
    /// A tuple: see [`Tuple`].
    Tuple,
    /// An enum: see [`Enum`].
    Enum,
    /// A growable sequence: see [`List`].
    List,
    /// A fixed-size array: see [`Array`].
    Array,
    /// A map: see [`Map`].
    Map,
    /// A set: see [`Set`].
    Set,
    /// A value reflection does not look into.
    Opaque,
}

impl fmt::Display for ReflectKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Struct => "struct",
            Self::TupleStruct => "tuple struct",
            Self::Tuple => "tuple",
            Self::Enum => "enum",
            Self::List => "list",
            Self::Array => "array",
            Self::Map => "map",
            Self::Set => "set",
            Self::Opaque => "opaque value",
        };
        f.write_str(name)
    }
}

/// A value that can be inspected and changed without knowing its type at compile time.
///
/// The trait is object safe: code that handles `&dyn Reflect` or `Box<dyn Reflect>` can ask for the
/// [`TypeInfo`], look at the value through [`Reflect::reflect_ref`] (which hands out the trait of its kind:
/// [`Struct`], [`List`], ...), change it through [`Reflect::reflect_mut`], or get the concrete type back
/// with `downcast_ref`.
///
/// `Reflect` needs `Send + Sync`: reflected values are meant for components and assets, which cross threads.
///
/// Implementing the trait by hand means writing [`Typed`] too and calling [`impl_reflect_boilerplate!`] in the
/// body of the `impl`, which writes the methods that look the same for every type. The derive macro of
/// milestone R2 will do all of it.
pub trait Reflect: Any + Send + Sync {
    /// The description of the type of this value.
    fn type_info(&self) -> &'static TypeInfo;

    /// The value as `&dyn Any`.
    fn as_any(&self) -> &dyn Any;

    /// The value as `&mut dyn Any`.
    fn as_any_mut(&mut self) -> &mut dyn Any;

    /// The boxed value as `Box<dyn Any>`.
    fn into_any(self: Box<Self>) -> Box<dyn Any>;

    /// The value as `&dyn Reflect`.
    fn as_reflect(&self) -> &dyn Reflect;

    /// The value as `&mut dyn Reflect`.
    fn as_reflect_mut(&mut self) -> &mut dyn Reflect;

    /// The boxed value as `Box<dyn Reflect>`.
    fn into_reflect(self: Box<Self>) -> Box<dyn Reflect>;

    /// Replaces the whole value with `value`, which must have the same type.
    ///
    /// # Errors
    ///
    /// [`ReflectError::TypeMismatch`] if `value` is of another type; the value is left as it was.
    fn set(&mut self, value: Box<dyn Reflect>) -> Result<(), ReflectError>;

    /// The value seen through the trait of its kind, for reading.
    fn reflect_ref(&self) -> ReflectRef<'_>;

    /// The value seen through the trait of its kind, for changing.
    fn reflect_mut(&mut self) -> ReflectMut<'_>;

    /// The kind of the value (what its [`TypeInfo`] says).
    fn kind(&self) -> ReflectKind {
        self.type_info().kind()
    }
}

impl dyn Reflect {
    /// Whether the value is a `T`.
    pub fn is<T: Reflect>(&self) -> bool {
        self.as_any().is::<T>()
    }

    /// The value as a `&T`, if it is a `T`.
    pub fn downcast_ref<T: Reflect>(&self) -> Option<&T> {
        self.as_any().downcast_ref::<T>()
    }

    /// The value as a `&mut T`, if it is a `T`.
    pub fn downcast_mut<T: Reflect>(&mut self) -> Option<&mut T> {
        self.as_any_mut().downcast_mut::<T>()
    }

    /// Moves the value out of the box, if it is a `T`.
    ///
    /// # Errors
    ///
    /// [`ReflectError::TypeMismatch`] if the value is of another type; the value is dropped.
    pub fn take<T: Typed>(self: Box<Self>) -> Result<T, ReflectError> {
        let actual = self.type_info().path();
        match self.into_any().downcast::<T>() {
            Ok(value) => Ok(*value),
            Err(_) => Err(ReflectError::type_mismatch(
                T::static_type_info().path(),
                actual,
            )),
        }
    }
}

/// A [`Reflect`] type whose description is known without a value: `T::static_type_info()`.
pub trait Typed: Reflect + Sized {
    /// The description of the type. The same `&'static TypeInfo` every time.
    ///
    /// Implementations keep it in a [`TypeInfoCell`](crate::TypeInfoCell) (non-generic types) or a
    /// [`GenericTypeInfoCell`](crate::GenericTypeInfoCell) (generic types).
    fn static_type_info() -> &'static TypeInfo;

    /// Adds the type data of this type (`ReflectDefault`, `ReflectClone`, ...) to its registration. The
    /// default adds none. The derive writes it from `#[reflect(Default, Clone, ...)]`; the standard types
    /// override it.
    fn register_type_data(_registration: &mut TypeRegistration) {}
}

/// Writes the methods of a [`Reflect`] implementation that are the same for every type: `type_info`,
/// the six conversions and `set`. The type must implement [`Typed`].
///
/// ```
/// use reflection::{
///     impl_reflect_boilerplate, Reflect, ReflectMut, ReflectRef, TypeInfo, TypeInfoCell, TypeKey, Typed,
/// };
///
/// #[derive(PartialEq, Debug)]
/// struct Meters(u8);
///
/// impl Typed for Meters {
///     fn static_type_info() -> &'static TypeInfo {
///         static CELL: TypeInfoCell = TypeInfoCell::new();
///         CELL.get_or_init(|| {
///             TypeInfo::Opaque(reflection::OpaqueInfo::new::<Meters>(TypeKey::new("example::Meters")))
///         })
///     }
/// }
///
/// impl Reflect for Meters {
///     impl_reflect_boilerplate!();
///
///     fn reflect_ref(&self) -> ReflectRef<'_> {
///         ReflectRef::Opaque(self)
///     }
///
///     fn reflect_mut(&mut self) -> ReflectMut<'_> {
///         ReflectMut::Opaque(self)
///     }
/// }
///
/// let mut value = Meters(1);
/// value.set(Box::new(Meters(5))).unwrap();
/// assert_eq!(value, Meters(5));
/// assert_eq!(value.type_info().path(), "example::Meters");
/// ```
#[macro_export]
macro_rules! impl_reflect_boilerplate {
    () => {
        fn type_info(&self) -> &'static $crate::TypeInfo {
            <Self as $crate::Typed>::static_type_info()
        }

        fn as_any(&self) -> &dyn ::std::any::Any {
            self
        }

        fn as_any_mut(&mut self) -> &mut dyn ::std::any::Any {
            self
        }

        fn into_any(self: ::std::boxed::Box<Self>) -> ::std::boxed::Box<dyn ::std::any::Any> {
            self
        }

        fn as_reflect(&self) -> &dyn $crate::Reflect {
            self
        }

        fn as_reflect_mut(&mut self) -> &mut dyn $crate::Reflect {
            self
        }

        fn into_reflect(self: ::std::boxed::Box<Self>) -> ::std::boxed::Box<dyn $crate::Reflect> {
            self
        }

        fn set(
            &mut self,
            value: ::std::boxed::Box<dyn $crate::Reflect>,
        ) -> ::std::result::Result<(), $crate::ReflectError> {
            let actual = $crate::Reflect::type_info(&*value).path();
            match $crate::Reflect::into_any(value).downcast::<Self>() {
                ::std::result::Result::Ok(value) => {
                    *self = *value;
                    ::std::result::Result::Ok(())
                }
                ::std::result::Result::Err(_) => {
                    ::std::result::Result::Err($crate::ReflectError::type_mismatch(
                        <Self as $crate::Typed>::static_type_info().path(),
                        actual,
                    ))
                }
            }
        }
    };
}

/// A value seen through the trait of its kind, for reading. Made by [`Reflect::reflect_ref`].
#[derive(Clone, Copy)]
pub enum ReflectRef<'a> {
    /// A struct with named fields.
    Struct(&'a dyn Struct),
    /// A tuple struct.
    TupleStruct(&'a dyn TupleStruct),
    /// A tuple.
    Tuple(&'a dyn Tuple),
    /// An enum.
    Enum(&'a dyn Enum),
    /// A growable sequence.
    List(&'a dyn List),
    /// A fixed-size array.
    Array(&'a dyn Array),
    /// A map.
    Map(&'a dyn Map),
    /// A set.
    Set(&'a dyn Set),
    /// A value reflection does not look into.
    Opaque(&'a dyn Reflect),
}

impl ReflectRef<'_> {
    /// The kind of the value.
    pub fn kind(&self) -> ReflectKind {
        match self {
            Self::Struct(_) => ReflectKind::Struct,
            Self::TupleStruct(_) => ReflectKind::TupleStruct,
            Self::Tuple(_) => ReflectKind::Tuple,
            Self::Enum(_) => ReflectKind::Enum,
            Self::List(_) => ReflectKind::List,
            Self::Array(_) => ReflectKind::Array,
            Self::Map(_) => ReflectKind::Map,
            Self::Set(_) => ReflectKind::Set,
            Self::Opaque(_) => ReflectKind::Opaque,
        }
    }
}

/// A value seen through the trait of its kind, for changing. Made by [`Reflect::reflect_mut`].
pub enum ReflectMut<'a> {
    /// A struct with named fields.
    Struct(&'a mut dyn Struct),
    /// A tuple struct.
    TupleStruct(&'a mut dyn TupleStruct),
    /// A tuple.
    Tuple(&'a mut dyn Tuple),
    /// An enum.
    Enum(&'a mut dyn Enum),
    /// A growable sequence.
    List(&'a mut dyn List),
    /// A fixed-size array.
    Array(&'a mut dyn Array),
    /// A map.
    Map(&'a mut dyn Map),
    /// A set.
    Set(&'a mut dyn Set),
    /// A value reflection does not look into.
    Opaque(&'a mut dyn Reflect),
}

impl ReflectMut<'_> {
    /// The kind of the value.
    pub fn kind(&self) -> ReflectKind {
        match self {
            Self::Struct(_) => ReflectKind::Struct,
            Self::TupleStruct(_) => ReflectKind::TupleStruct,
            Self::Tuple(_) => ReflectKind::Tuple,
            Self::Enum(_) => ReflectKind::Enum,
            Self::List(_) => ReflectKind::List,
            Self::Array(_) => ReflectKind::Array,
            Self::Map(_) => ReflectKind::Map,
            Self::Set(_) => ReflectKind::Set,
            Self::Opaque(_) => ReflectKind::Opaque,
        }
    }
}

/// Access to the fields of a struct with named fields.
pub trait Struct: Reflect {
    /// The number of fields.
    fn field_len(&self) -> usize;

    /// The name of the field at `index`.
    fn name_at(&self, index: usize) -> Option<&'static str>;

    /// The field with the given name.
    fn field(&self, name: &str) -> Option<&dyn Reflect>;

    /// The field with the given name, for changing.
    fn field_mut(&mut self, name: &str) -> Option<&mut dyn Reflect>;

    /// The field at `index` (declaration order).
    fn field_at(&self, index: usize) -> Option<&dyn Reflect>;

    /// The field at `index`, for changing.
    fn field_at_mut(&mut self, index: usize) -> Option<&mut dyn Reflect>;
}

/// Access to the fields of a tuple struct.
pub trait TupleStruct: Reflect {
    /// The number of fields.
    fn field_len(&self) -> usize;

    /// The field at `index`.
    fn field(&self, index: usize) -> Option<&dyn Reflect>;

    /// The field at `index`, for changing.
    fn field_mut(&mut self, index: usize) -> Option<&mut dyn Reflect>;
}

/// Access to the fields of a tuple.
pub trait Tuple: Reflect {
    /// The number of fields.
    fn field_len(&self) -> usize;

    /// The field at `index`.
    fn field(&self, index: usize) -> Option<&dyn Reflect>;

    /// The field at `index`, for changing.
    fn field_mut(&mut self, index: usize) -> Option<&mut dyn Reflect>;
}

/// Access to an enum: the current variant and its fields.
///
/// Changing the variant of an enum in place comes with the dynamic values of milestone R4; until then
/// [`Reflect::set`] replaces the whole value.
pub trait Enum: Reflect {
    /// The name of the current variant.
    fn variant_name(&self) -> &'static str;

    /// The position of the current variant (declaration order, starting at 0).
    fn variant_index(&self) -> usize;

    /// The kind of fields the current variant has.
    fn variant_kind(&self) -> VariantKind;

    /// The number of fields of the current variant.
    fn field_len(&self) -> usize;

    /// The name of the field at `index` of the current variant (variants with named fields only).
    fn name_at(&self, index: usize) -> Option<&'static str>;

    /// The field of the current variant with the given name.
    fn field(&self, name: &str) -> Option<&dyn Reflect>;

    /// The field of the current variant with the given name, for changing.
    fn field_mut(&mut self, name: &str) -> Option<&mut dyn Reflect>;

    /// The field of the current variant at `index`.
    fn field_at(&self, index: usize) -> Option<&dyn Reflect>;

    /// The field of the current variant at `index`, for changing.
    fn field_at_mut(&mut self, index: usize) -> Option<&mut dyn Reflect>;
}

/// Access to a growable sequence.
pub trait List: Reflect {
    /// The number of items.
    fn len(&self) -> usize;

    /// Whether the list has no items.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The item at `index`.
    fn get(&self, index: usize) -> Option<&dyn Reflect>;

    /// The item at `index`, for changing.
    fn get_mut(&mut self, index: usize) -> Option<&mut dyn Reflect>;

    /// Appends an item.
    ///
    /// # Errors
    ///
    /// [`ReflectError::TypeMismatch`] if `value` is not of the item type.
    fn push(&mut self, value: Box<dyn Reflect>) -> Result<(), ReflectError>;

    /// Inserts an item at `index`, moving the later items up.
    ///
    /// # Errors
    ///
    /// [`ReflectError::IndexOutOfBounds`] if `index` is greater than the length,
    /// [`ReflectError::TypeMismatch`] if `value` is not of the item type.
    fn insert(&mut self, index: usize, value: Box<dyn Reflect>) -> Result<(), ReflectError>;

    /// Removes and returns the last item.
    fn pop(&mut self) -> Option<Box<dyn Reflect>>;

    /// Removes and returns the item at `index`, moving the later items down.
    fn remove(&mut self, index: usize) -> Option<Box<dyn Reflect>>;

    /// Removes every item.
    fn clear(&mut self);

    /// The items, in order.
    fn iter(&self) -> ReflectIter<'_, &dyn Reflect>;
}

/// Access to a fixed-size array.
pub trait Array: Reflect {
    /// The number of items.
    fn len(&self) -> usize;

    /// Whether the array has no items.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The item at `index`.
    fn get(&self, index: usize) -> Option<&dyn Reflect>;

    /// The item at `index`, for changing.
    fn get_mut(&mut self, index: usize) -> Option<&mut dyn Reflect>;

    /// The items, in order.
    fn iter(&self) -> ReflectIter<'_, &dyn Reflect>;
}

/// Access to a map. Keys and values are given and returned as reflected values; a key of the wrong type
/// finds nothing.
pub trait Map: Reflect {
    /// The number of entries.
    fn len(&self) -> usize;

    /// Whether the map has no entries.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The value for `key`.
    fn get(&self, key: &dyn Reflect) -> Option<&dyn Reflect>;

    /// The value for `key`, for changing.
    fn get_mut(&mut self, key: &dyn Reflect) -> Option<&mut dyn Reflect>;

    /// Inserts an entry and returns the value it replaced.
    ///
    /// # Errors
    ///
    /// [`ReflectError::TypeMismatch`] if the key or the value is not of the map's type; the map is not
    /// changed.
    fn insert(
        &mut self,
        key: Box<dyn Reflect>,
        value: Box<dyn Reflect>,
    ) -> Result<Option<Box<dyn Reflect>>, ReflectError>;

    /// Removes the entry for `key` and returns its value.
    fn remove(&mut self, key: &dyn Reflect) -> Option<Box<dyn Reflect>>;

    /// Removes every entry.
    fn clear(&mut self);

    /// The entries. The order is the map's own: unspecified for `HashMap`, ascending for `BTreeMap`.
    fn iter(&self) -> ReflectIter<'_, (&dyn Reflect, &dyn Reflect)>;
}

/// Access to a set. Items cannot be changed in place.
pub trait Set: Reflect {
    /// The number of items.
    fn len(&self) -> usize;

    /// Whether the set has no items.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether the set contains `value`; a value of the wrong type is never contained.
    fn contains(&self, value: &dyn Reflect) -> bool;

    /// Inserts an item; `true` if it was not there yet.
    ///
    /// # Errors
    ///
    /// [`ReflectError::TypeMismatch`] if `value` is not of the item type.
    fn insert(&mut self, value: Box<dyn Reflect>) -> Result<bool, ReflectError>;

    /// Removes an item; `true` if it was there.
    fn remove(&mut self, value: &dyn Reflect) -> bool;

    /// Removes every item.
    fn clear(&mut self);

    /// The items. The order is the set's own: unspecified for `HashSet`, ascending for `BTreeSet`.
    fn iter(&self) -> ReflectIter<'_, &dyn Reflect>;
}
