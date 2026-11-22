//! `reflection` — runtime type information for the Coreverse Engine: what a type is and what it is made
//! of, and a way to read and change values without knowing their type at compile time.
//!
//! The crate is built in milestones (see `docs/src/reflection.md`). This is **R2**: the core model (R1) and
//! `#[derive(Reflect)]`. There is no registry yet (R3), no paths or dynamic values (R4).
//!
//! # What is here today
//!
//! * [`TypeKey`]: the stable identity of a type, a path plus the FNV-1a hash of the path. It does not
//!   change from build to build, unlike `std::any::TypeId`, so it may be written to files.
//! * [`TypeInfo`]: the `'static` description of a type, one of [`StructInfo`], [`TupleStructInfo`],
//!   [`TupleInfo`], [`EnumInfo`], [`ListInfo`], [`ArrayInfo`], [`MapInfo`], [`SetInfo`] or [`OpaqueInfo`]
//!   (the [`ReflectKind`]s). Fields and variants name their types through [`TypeRef`], which builds the
//!   other description only when asked, so recursive types work.
//! * [`Reflect`](trait@Reflect): the object-safe trait of reflected values (`&dyn Reflect`, `Box<dyn Reflect>`):
//!   [`type_info`](Reflect::type_info), downcasts, [`set`](Reflect::set), and
//!   [`reflect_ref`](Reflect::reflect_ref) / [`reflect_mut`](Reflect::reflect_mut), which return the trait of
//!   the value's kind: [`Struct`], [`TupleStruct`], [`Tuple`], [`Enum`], [`List`], [`Array`], [`Map`],
//!   [`Set`], or the value itself for [`ReflectKind::Opaque`].
//! * [`Typed`]: the description of a type without a value, `T::static_type_info()`.
//! * Implementations for the integer and float types, `bool`, `char`, `String` and `()` (opaque values),
//!   tuples of up to eight fields, `[T; N]`, `Vec<T>`, `Option<T>` (an enum), `Box<T>` (a tuple struct with
//!   one field), `HashMap`, `BTreeMap`, `HashSet` and `BTreeSet`.
//! * `#[derive(Reflect)]` (from the `reflection-macros` crate, re-exported here as `Reflect`, next to the
//!   trait): writes `Typed`, `Reflect` and the trait of the kind for structs, tuple structs, unit structs and
//!   enums, generic ones included. See the attributes below.
//! * [`impl_reflect_boilerplate!`], [`TypeInfoCell`] and [`GenericTypeInfoCell`]: what an implementation
//!   written by hand needs; the derive uses all three.
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
//! # Deriving
//!
//! ```
//! use reflection::{Reflect, ReflectMut, ReflectRef, Typed};
//!
//! #[derive(Reflect)]
//! struct Player {
//!     name: String,
//!     #[reflect(rename = "hp", alias = "health")]
//!     health: u32,
//!     #[reflect(skip)]
//!     cache: Vec<u8>,
//! }
//!
//! #[derive(Reflect)]
//! enum Shape {
//!     Empty,
//!     Circle(f32),
//!     Rect { w: f32, h: f32 },
//! }
//!
//! let mut player = Player { name: "Ada".into(), health: 10, cache: Vec::new() };
//! if let ReflectMut::Struct(view) = Reflect::reflect_mut(&mut player) {
//!     assert_eq!(view.field_len(), 2); // `cache` is skipped
//!     assert_eq!(view.name_at(1), Some("hp"));
//!     *view.field_mut("health").unwrap().downcast_mut::<u32>().unwrap() = 20; // an alias works
//! }
//! assert_eq!(player.health, 20);
//!
//! let shape = Shape::Rect { w: 1.0, h: 2.0 };
//! let ReflectRef::Enum(view) = Reflect::reflect_ref(&shape) else { unreachable!() };
//! assert_eq!(view.variant_name(), "Rect");
//! assert_eq!(view.field("h").unwrap().downcast_ref::<f32>(), Some(&2.0));
//! assert!(Shape::static_type_info().path().ends_with("Shape"));
//! ```
//!
//! The path of a type (its [`TypeKey`]) defaults to `module_path!()` plus the name, so moving a type to
//! another module changes it; `#[reflect(name = "game::Player")]` pins it and `#[reflect(alias = "...")]`
//! records an old path. On fields: `skip`, `rename = "..."`, `alias = "..."` (named fields only). On
//! variants: `rename`, `alias`. Generic types get a `Typed` bound per type parameter and a path that contains
//! the paths of the arguments; lifetime parameters, unions and enums without variants are errors.
//!
//! Wrong uses fail to compile:
//!
//! ```compile_fail
//! use reflection::Reflect;
//!
//! #[derive(Reflect)]
//! union Bits {
//!     a: u32,
//!     b: f32,
//! }
//! ```
//!
//! ```compile_fail
//! use reflection::Reflect;
//!
//! #[derive(Reflect)]
//! struct Borrowed<'a> {
//!     text: &'a str,
//! }
//! ```
//!
//! ```compile_fail
//! use reflection::Reflect;
//!
//! #[derive(Reflect)]
//! struct Player {
//!     #[reflect(frobnicate)]
//!     health: u32,
//! }
//! ```
//!
//! # Conventions
//!
//! * `std`, `utilities` (the name hash and poison-tolerant locks) and `ccore` (`ReflectError` converts into
//!   `CoreError`); no `unsafe` code. Reflected values are `Send + Sync`.
//! * Descriptions are leaked once per generic instantiation that is used; they live as long as the program.
//! * Reflection does not serialize anything; formats belong to the `serialization` crate.

// Lets the code that the derive writes (`::reflection::...`) work inside this crate too.
extern crate self as reflection;

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
pub use reflection_macros::Reflect;
