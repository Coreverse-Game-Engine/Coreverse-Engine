//! `reflection` — runtime type information for the Coreverse Engine: what a type is and what it is made
//! of, and a way to read and change values without knowing their type at compile time.
//!
//! The crate is built in milestones (see `docs/src/reflection.md`). This is **R5**: the core model (R1),
//! `#[derive(Reflect)]` (R2), the type registry and type data (R3), paths, `apply`, structural clone and
//! equality, dynamic values and `from_reflect` (R4), metadata, typed custom attributes and the visitor (R5).
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
//! * [`TypeRegistry`]: the types a program knows, found by `TypeId`, path, alias or hash, with their
//!   [`TypeRegistration`]s; filled by hand with [`register`](TypeRegistry::register) or from every derived
//!   non-generic type with [`TypeRegistry::from_inventory`].
//! * Type data ([`ReflectDefault`], [`ReflectClone`], [`ReflectPartialEq`], [`ReflectDebug`],
//!   [`ReflectHash`], or your own through [`FromType`]): function tables that let code holding only a
//!   `&dyn Reflect` or a type name use `Default`, `Clone`, `PartialEq`, `Debug` and `Hash`.
//! * Paths ([`ReflectPath`], [`GetPath`]): `player.get_path::<u32>("stats.health")`, `items[2].0`.
//! * [`Reflect::apply`], [`Reflect::clone_value`] and [`Reflect::reflect_eq`]: structural copy, snapshot and
//!   comparison, built on dynamic values ([`DynamicStruct`], [`DynamicList`], ...), which can also be written
//!   by hand as patches. [`Typed::from_reflect`] builds a concrete value back from a reflected one.
//! * [`Metadata`]: tooltip, category, numeric [`ValueRange`], read-only / hidden / transient flags,
//!   documentation and typed custom attributes of a type, field or variant, read from its description
//!   (`field.metadata()`); set with `#[reflect(...)]` attributes.
//! * [`visit`] and [`ReflectVisitor`]: a depth-first walk over a value that tells the visitor the path, the
//!   field description and its metadata at every step; the base for serialization and editors.
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
//! # Metadata and visiting
//!
//! Besides `skip`, `rename`, `alias` and `default`, the attributes below describe things for tools. They are
//! plain information: reflection itself (`apply`, `path_set`, `reflect_mut`) ignores them.
//!
//! * On types, fields and variants: `tooltip = "..."`, `category = "..."`, `hidden`, and `attr = <expression>`
//!   (a typed custom attribute: any `Send + Sync + 'static` value, found again with `metadata.custom::<T>()`;
//!   may be repeated, one value per type).
//! * On types and fields: `readonly` and `transient` (a serializer should not write it; unlike `skip` it stays
//!   visible to reflection).
//! * On fields: `range(min = .., max = ..)`, numbers that convert into `f64`.
//! * Doc comments are captured as `metadata.docs()` when the `docs` feature of this crate is on.
//!
//! ```
//! use reflection::{Reflect, ReflectVisitor, VisitControl, VisitNode, Typed, visit};
//! use std::convert::Infallible;
//!
//! struct Slider {
//!     step: f32,
//! }
//!
//! #[derive(Reflect)]
//! #[reflect(category = "Gameplay")]
//! struct Health {
//!     #[reflect(range(min = 0.0, max = 100.0), tooltip = "Current hit points")]
//!     current: f32,
//!     #[reflect(transient, attr = Slider { step: 0.5 })]
//!     regen: f32,
//! }
//!
//! let info = Health::static_type_info().as_struct().unwrap();
//! assert_eq!(info.metadata().category(), Some("Gameplay"));
//! let current = info.field("current").unwrap().metadata();
//! assert_eq!(current.range().unwrap().max, 100.0);
//! let regen = info.field("regen").unwrap().metadata();
//! assert!(regen.is_transient());
//! assert_eq!(regen.custom::<Slider>().unwrap().step, 0.5);
//!
//! // A serializer-like walk that leaves out transient fields.
//! struct Names(Vec<String>);
//! impl ReflectVisitor for Names {
//!     type Error = Infallible;
//!     fn enter(&mut self, node: &VisitNode<'_>, _: &dyn Reflect) -> Result<VisitControl, Infallible> {
//!         if node.metadata().is_some_and(|metadata| metadata.is_transient()) {
//!             return Ok(VisitControl::SkipChildren);
//!         }
//!         self.0.push(node.path().to_owned());
//!         Ok(VisitControl::Continue)
//!     }
//! }
//! let mut names = Names(Vec::new());
//! visit(&Health { current: 1.0, regen: 2.0 }, &mut names).unwrap();
//! assert_eq!(names.0, ["", "current"]);
//! ```
//!
//! # The registry
//!
//! `#[reflect(Default, Clone, PartialEq, Debug, Hash)]` on a type stores the matching type data in its
//! registration (any other uppercase name is a type data of your own: `<Name as FromType<Type>>::from_type()`
//! is called; not allowed on generic types, use [`TypeRegistry::add_type_data`] there). The standard types
//! come with the data of the traits they implement.
//!
//! ```
//! use reflection::{Reflect, TypeRegistry};
//!
//! #[derive(Reflect, Default, Debug, PartialEq)]
//! #[reflect(Default)]
//! struct Settings {
//!     volume: u32,
//! }
//!
//! let mut registry = TypeRegistry::new();
//! registry.register::<Settings>().unwrap();
//! let path = registry.get_by_type::<Settings>().unwrap().path();
//! let value = registry.create_default(path).unwrap();
//! assert_eq!(value.downcast_ref::<Settings>(), Some(&Settings { volume: 0 }));
//! ```
//!
//! Registering a type registers the types it is made of; two different types may not share a path or an
//! alias ([`ReflectError::NameConflict`]). Every non-generic derived type also submits itself to
//! `inventory`, so `TypeRegistry::from_inventory()` finds all of them without a list.
//!
//! # Paths, snapshots and patches
//!
//! ```
//! use reflection::{DynamicStruct, GetPath, Reflect, Typed};
//!
//! #[derive(Reflect, Debug, PartialEq, Clone)]
//! struct Stats {
//!     health: u32,
//!     mana: u32,
//! }
//!
//! #[derive(Reflect, Debug, PartialEq, Clone)]
//! struct Player {
//!     name: String,
//!     stats: Stats,
//!     items: Vec<String>,
//! }
//!
//! let mut player = Player {
//!     name: "Ada".into(),
//!     stats: Stats { health: 10, mana: 5 },
//!     items: vec!["axe".into()],
//! };
//! assert_eq!(player.get_path::<String>("items[0]").unwrap(), "axe");
//!
//! // A snapshot made of dynamic values, applied back later (undo).
//! let snapshot = player.clone_value().unwrap();
//! *player.get_path_mut::<u32>("stats.health").unwrap() = 99;
//! player.items.push("bow".into());
//! assert_eq!(player.reflect_eq(&*snapshot), Some(false));
//! player.apply(&*snapshot).unwrap();
//! assert_eq!(player.stats.health, 10);
//! assert_eq!(player.items.len(), 1);
//!
//! // A patch: only the fields it has are changed.
//! let mut stats = DynamicStruct::new();
//! stats.insert("mana", 7_u32);
//! let mut patch = DynamicStruct::new();
//! patch.insert("stats", stats);
//! player.apply(&patch).unwrap();
//! assert_eq!(player.stats, Stats { health: 10, mana: 7 });
//!
//! // And back to a concrete value.
//! let copy = Player::from_reflect(&*player.clone_value().unwrap()).unwrap();
//! assert_eq!(copy, player);
//! ```
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
//! ```compile_fail
//! use reflection::Reflect;
//!
//! // `range` is only for fields
//! #[derive(Reflect)]
//! #[reflect(range(min = 0, max = 1))]
//! struct Player {
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
mod dynamic;
mod error;
mod impls;
mod info;
mod key;
mod metadata;
mod ops;
mod path;
mod reflect;
mod registry;
mod type_data;
mod visitor;

pub use cell::{GenericTypeInfoCell, TypeInfoCell};
pub use dynamic::{
    DynamicArray, DynamicEnum, DynamicList, DynamicMap, DynamicSet, DynamicStruct, DynamicTuple,
    DynamicTupleStruct, DynamicVariant,
};
pub use error::ReflectError;
pub use info::{
    ArrayInfo, EnumInfo, FieldInfo, ListInfo, MapInfo, OpaqueInfo, SetInfo, StructInfo, TupleInfo,
    TupleStructInfo, TypeInfo, TypeRef, VariantInfo, VariantKind,
};
pub use key::TypeKey;
pub use metadata::{Metadata, ValueRange};
pub use path::{GetPath, PathSegment, ReflectPath};
pub use reflect::{
    Array, Enum, List, Map, Reflect, ReflectIter, ReflectKind, ReflectMut, ReflectRef, Set, Struct,
    Tuple, TupleStruct, Typed,
};
pub use reflection_macros::Reflect;
pub use registry::{TypeRegistration, TypeRegistry};
pub use type_data::{
    DebugValue, FromType, ReflectClone, ReflectDebug, ReflectDefault, ReflectFromReflect,
    ReflectHash, ReflectPartialEq, TypeData,
};
pub use visitor::{ReflectVisitor, VisitControl, VisitNode, VisitSlot, visit};

/// What the code written by `#[derive(Reflect)]` needs; not part of the API.
#[doc(hidden)]
pub mod __private {
    pub use crate::registry::TypeEntry;
    pub use inventory;
}
