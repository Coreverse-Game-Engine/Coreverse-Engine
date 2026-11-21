//! `reflection` — runtime type information for the Coreverse Engine: what a type is and what it is made
//! of, and a way to read and change values without knowing their type at compile time.
//!
//! The crate is built in milestones (see `docs/src/reflection.md`). This is **R1**, the core model; there is
//! no derive macro yet (R2), no registry (R3), no paths or dynamic values (R4).
//!
//! # What is here today
//!
//! * [`TypeKey`]: the stable identity of a type, a path plus the FNV-1a hash of the path. It does not
//!   change from build to build, unlike `std::any::TypeId`, so it may be written to files.
//! * [`TypeInfo`]: the `'static` description of a type, one of [`StructInfo`], [`TupleStructInfo`],
//!   [`TupleInfo`], [`EnumInfo`], [`ListInfo`], [`ArrayInfo`], [`MapInfo`], [`SetInfo`] or [`OpaqueInfo`]
//!   (the [`ReflectKind`]s). Fields and variants name their types through [`TypeRef`], which builds the
//!   other description only when asked, so recursive types work.
//! * [`Reflect`]: the object-safe trait of reflected values (`&dyn Reflect`, `Box<dyn Reflect>`):
//!   [`type_info`](Reflect::type_info), downcasts, [`set`](Reflect::set), and
//!   [`reflect_ref`](Reflect::reflect_ref) / [`reflect_mut`](Reflect::reflect_mut), which return the trait of
//!   the value's kind: [`Struct`], [`TupleStruct`], [`Tuple`], [`Enum`], [`List`], [`Array`], [`Map`],
//!   [`Set`], or the value itself for [`ReflectKind::Opaque`].
//! * [`Typed`]: the description of a type without a value, `T::static_type_info()`.
//! * Implementations for the integer and float types, `bool`, `char`, `String` and `()` (opaque values),
//!   tuples of up to eight fields, `[T; N]`, `Vec<T>`, `Option<T>` (an enum), `Box<T>` (a tuple struct with
//!   one field), `HashMap`, `BTreeMap`, `HashSet` and `BTreeSet`.
//! * [`impl_reflect_boilerplate!`], [`TypeInfoCell`] and [`GenericTypeInfoCell`]: what an implementation
//!   written by hand needs. The derive macro of R2 will write all of it.
//!
//! ```
//! use reflection::{Reflect, ReflectMut, Typed};
//!
//! let mut numbers: Vec<u32> = vec![1, 2];
//! assert_eq!(Vec::<u32>::static_type_info().path(), "Vec<u32>");
//!
//! let value: &mut dyn Reflect = &mut numbers;
//! match value.reflect_mut() {
//!     ReflectMut::List(list) => list.push(Box::new(3u32)).unwrap(),
//!     _ => unreachable!(),
//! }
//! assert_eq!(numbers, [1, 2, 3]);
//! ```
//!
//! # Conventions
//!
//! * `std`, `utilities` (the name hash and poison-tolerant locks) and `ccore` (`ReflectError` converts into
//!   `CoreError`); no `unsafe` code. Reflected values are `Send + Sync`.
//! * Descriptions are leaked once per generic instantiation that is used; they live as long as the program.
//! * Reflection does not serialize anything; formats belong to the `serialization` crate.

mod cell;
mod error;
mod impls;
mod info;
mod key;
mod reflect;

pub use cell::{GenericTypeInfoCell, TypeInfoCell};
pub use error::ReflectError;
pub use info::{
    ArrayInfo, EnumInfo, FieldInfo, ListInfo, MapInfo, OpaqueInfo, SetInfo, StructInfo, TupleInfo,
    TupleStructInfo, TypeInfo, TypeRef, VariantInfo, VariantKind,
};
pub use key::TypeKey;
pub use reflect::{
    Array, Enum, List, Map, Reflect, ReflectIter, ReflectKind, ReflectMut, ReflectRef, Set, Struct,
    Tuple, TupleStruct, Typed,
};
