# Reflection

`engine/rust/crates/reflection` is the engine's runtime type information: what a type is, what it is made of, and
a way to read and change values without knowing their type at compile time. It is being built in milestones; this
page describes what exists (R1, the core model, R2, `#[derive(Reflect)]`, R3, the type registry and type data, and
R4, paths, `apply`, structural clone and equality, dynamic values and `from_reflect`, and R5, metadata, typed custom
attributes and the visitor) and what is planned. The crate uses `std`, `utilities` (the name
hash and poison-tolerant locks) and `ccore` (`ReflectError` converts into `CoreError`), and contains no `unsafe`
code.

> **Verification status.** R1, R2 and R3 were verified by the maintainer (`cargo fmt`, `cargo clippy --workspace
> --all-features --all-targets -- -D warnings`, `cargo test`, Miri). R4 and R5 were written in a sandbox without a
> Rust toolchain; R5 has not been compiled, linted or tested yet: run the same four commands before merging it (and
> `cargo test -p reflection` once without `--all-features`, which is the only way to run the tests of the
> no-`docs` case).

## What is there (R1)

| Item | Purpose |
|:-----|:--------|
| `TypeKey` | The stable identity of a type: a path (`"u32"`, `"Vec<String>"`, `"game::Player"`) and its 64-bit FNV-1a hash. Unlike `std::any::TypeId` and `type_name`, it does not change from build to build, so it may be written to files. `TypeKey::new` is `const`; `TypeKey::from_owned` interns a path built at run time. |
| `TypeInfo` | The `'static` description of a type. One variant per kind: `StructInfo`, `TupleStructInfo`, `TupleInfo`, `EnumInfo`, `ListInfo`, `ArrayInfo`, `MapInfo`, `SetInfo`, `OpaqueInfo`. `key()`, `path()`, `id()`, `kind()`, `is::<T>()`, `as_struct()` and the other `as_*` accessors. |
| `FieldInfo`, `VariantInfo`, `VariantKind` | Fields (name or none, position, type) and enum variants (name, position, `Unit`/`Tuple`/`Struct`, fields). |
| `TypeRef` | A lazy reference to another type's description: `TypeRef::of::<T>()` stores a function pointer, `info()` builds the description on first use. This is what keeps recursive types from looping. |
| `Reflect` | The object-safe trait of reflected values (`&dyn Reflect`, `Box<dyn Reflect>`; `Send + Sync`): `type_info()`, `kind()`, `set(Box<dyn Reflect>)`, `reflect_ref()`, `reflect_mut()`, the `as_any`/`into_any`/`as_reflect` conversions. `dyn Reflect` has `is`, `downcast_ref`, `downcast_mut` and `take::<T>()`. |
| `ReflectRef`, `ReflectMut` | What `reflect_ref()` and `reflect_mut()` return: the value seen through the trait of its kind. |
| `Struct`, `TupleStruct`, `Tuple`, `Enum` | Field access by name and/or position; for enums also the current variant (`variant_name`, `variant_index`, `variant_kind`). |
| `List`, `Array`, `Map`, `Set` | Containers: `len`, `get`/`get_mut`, `push`/`insert`/`pop`/`remove`/`clear` (lists), `iter`. Values go in as `Box<dyn Reflect>`; a value of the wrong type is a `ReflectError::TypeMismatch`, a key of the wrong type finds nothing. |
| `Typed` | The description of a type without a value: `T::static_type_info()`. |
| `aliases()` | Every description (`TypeInfo` and the info structs), `FieldInfo` and `VariantInfo` carry the other paths or names they were known by (set with `with_aliases`); empty unless the derive's `alias` attribute was used. Nothing resolves aliases yet except `field`/`field_mut` of derived types; lookups by alias in the registry arrive with R3. |
| `ReflectError` | `TypeMismatch { expected, actual }` and `IndexOutOfBounds { index, len }`; `Display`, `Error`, `From<ReflectError> for CoreError`; `#[non_exhaustive]`. |
| `impl_reflect_boilerplate!`, `TypeInfoCell`, `GenericTypeInfoCell` | What a hand-written implementation needs. The macro writes `type_info`, the six conversions and `set`; the cells hold the `'static` description (one per type, or one per instantiation of a generic type). The derive uses all three. |

Implemented for: the integer and float types, `bool`, `char`, `String`, `()` (opaque values); tuples of one to eight
fields; `[T; N]`; `Vec<T>`; `Option<T>` (an enum with the variants `None` and `Some`); `Box<T>`; `HashMap<K, V>` and
`BTreeMap<K, V>`; `HashSet<T>` and `BTreeSet<T>`. Every type argument must itself be `Typed`.

## Design decisions

* **Safe code only.** Fields are reached through `Any` downcasts and ordinary `&`/`&mut` borrows; there is no offset
  arithmetic. The price: two fields of one value cannot be borrowed mutably at the same time through reflection.
* **Own type keys.** The path of a built-in type is fixed (`"u32"`, `"Vec<T>"` with `T` replaced by the item type's
  path, ...); it does not follow the standard library's module layout. A user type chooses its own path (the derive
  defaults to `module_path!()` plus the name, with `#[reflect(name = ...)]` and `alias` for renames).
  Collisions between two registered types are detected when the registry arrives (R3).
* **Descriptions are `'static` and lazy.** A non-generic type keeps its description in a `TypeInfoCell` (a
  `OnceLock`). Generic types share one `static` per implementation, so `GenericTypeInfoCell` keeps a map from the
  standard `TypeId` of the instantiation to a leaked description. At most one description per used instantiation is
  leaked, and a lost race drops its copy. No lock is held while a description is built.
* **No eager references.** A field's type is a `TypeRef`, not a description. Building a description never builds
  another one except through the type arguments of a generic path (`Option<Box<Node>>` needs the path of
  `Box<Node>`, which needs the path of `Node`), so a description never asks for itself.
* **`Box<T>` is a tuple struct with one field**, not a new kind. Serializers and inspectors that do not care about
  the indirection step through it. `Rc` and `Arc` are not implemented (`Reflect` requires `Send + Sync`; `Arc` waits
  for R6).
* **`Option<T>` is an enum.** How it is written (`null`, a tagged union, ...) is up to the serializer.
* **Reflection does not serialize.** It only describes and gives access; formats belong to the `serialization`
  crate, which walks values through the visitor of R5.
* **`Debug`, `Clone`, `PartialEq`, `Default` and `Hash` of reflected values** go through the type data of R3, not
  through the `Reflect` trait; the structural versions (`reflect_eq`, `clone_value`, `apply`) of R4 work without a
  registry.

## Deriving (R2)

`#[derive(Reflect)]` lives in `engine/rust/macros/reflection-macros` (a proc-macro crate next to `math-macros` and
`vfs-macros`) and is re-exported by `reflection` under the same name as the trait: `use reflection::Reflect;`
brings in both. The generated code names `::reflection::...`, so the crate that uses the derive depends on
`reflection` (inside `reflection` itself, `extern crate self as reflection;` makes the same paths work).

It writes `Typed`, `Reflect` (starting with `impl_reflect_boilerplate!()`) and the trait of the kind:

| Input | Reflected as |
|:------|:-------------|
| `struct S { a: A, b: B }` and `struct S;` | `Struct` (a unit struct has no fields) |
| `struct S(A, B)` | `TupleStruct` |
| `enum E { Unit, Tuple(A), Named { a: A } }` | `Enum` with unit, tuple and struct variants |
| any of these with type or const parameters | the same, with a `Typed` bound added per type parameter |

Attributes, all `#[reflect(...)]`:

| On | Attribute | Effect |
|:---|:----------|:-------|
| type | `name = "game::Player"` | The path of the type. Without it the path is `module_path!()` plus the name (`game::player::Player`), so moving the type to another module changes it. For a generic type this is the part before the arguments. |
| type | `alias = "game::OldPlayer"` | Another path the type was known by, recorded in `TypeInfo::aliases()`. Repeatable. Not allowed on generic types. |
| field | `skip` | The field is not reflected: not in the description, not reachable, not counted. Its type does not need to be `Reflect`. |
| named field | `rename = "hp"` | The reflected name. |
| named field | `alias = "health"` | Another name; `field` and `field_mut` accept it, `name_at` and the description give the real name. Repeatable. |
| variant | `rename`, `alias` | As on a named field. |

Compile-time errors (spanned at the offending input): unions, enums without variants, lifetime parameters,
unknown `reflect` attributes, empty strings, `rename`/`alias` on an unnamed field, `alias` on a generic type, and
two fields (or two variants) that end up with the same name or alias.

The path of a generic instantiation contains the paths of its arguments: `derive::Pair<u32,String>`; a const
argument is written with its value (`derive::Buf<4>`). Reflected indices count reflected fields only, so with a
skipped field in the middle of a tuple struct, `field(1)` is the third field.

Derive-specific decisions:

* The output is plain safe code: `match` on the field name or index, `&self.field as &dyn Reflect`; enum variants
  are matched with `Self::Variant(..)` patterns, and every `match self` has one arm per variant (no wildcard).
* Aliases are accepted by `field`/`field_mut` but not by `name_at` or the description's own lookups
  (`StructInfo::field`), which use the real name. A derive does not generate `Default`; the `Typed::from_reflect` it
  writes (R4) fills skipped fields with `Default::default()`, so a skipped field has to be `Default` or the type has
  to opt out with `#[reflect(no_from_reflect)]`; `#[reflect(default)]` on a field does the same for a field the
  source lacks.
* An enum cannot change its variant in place; `apply` rebuilds the value with `Reflect::try_replace` (R4) instead.

## The registry and type data (R3)

| Item | Purpose |
|:-----|:--------|
| `TypeRegistry` | The types a program knows how to reflect. `register::<T>()` registers `T` and everything it is made of (field types, variant fields, item, key and value types) with the type data each asks for; registering a type that is there is a no-op; a failure registers nothing. `from_inventory()` registers every derived non-generic type. Lookups: `get(TypeId)`, `get_by_type::<T>()`, `contains::<T>()`, `get_by_path(&str)` (path **or alias**), `get_by_id(u64)`, `get_by_key(&TypeKey)`, `data::<T, D>()`, `iter()`, `len()`. Helpers: `create_default(name)` and `clone_value(&dyn Reflect)`. `add_type_data::<T, D>(data)` registers `T` if needed and adds or replaces its data. There is no global registry. |
| `TypeRegistration` | One registered type: its `TypeInfo` and a bag of type data keyed by the type data's `TypeId`: `info()`, `key()`, `path()`, `insert_data`, `data::<D>()`, `data_mut`, `has_data`, `remove_data`, `data_len`. `TypeRegistration::of::<T>()` is `T`'s description plus `T::register_type_data`. |
| `TypeData`, `FromType<T>` | Type data is any `Any + Send + Sync` value. `FromType<T>` builds the type data `Self` for `T`; it is what `#[reflect(Name)]` calls. |
| `ReflectDefault` | `create() -> Box<dyn Reflect>` for a `Default` type. |
| `ReflectClone` | `clone_value(&dyn Reflect) -> Result<Box<dyn Reflect>, ReflectError>` for a `Clone` type. |
| `ReflectPartialEq` | `eq(&dyn Reflect, &dyn Reflect) -> Result<bool, ReflectError>`. |
| `ReflectDebug` | `debug(&dyn Reflect)` returns a value that prints with the type's `Debug` (a value of another type prints `<type mismatch>`). |
| `ReflectHash` | `hash_value(&dyn Reflect, &mut dyn Hasher) -> Result<(), ReflectError>`. |
| `Typed::register_type_data` | A default method that does nothing; the derive and the standard types override it. |
| `ReflectError` | New variants `NameConflict { name, existing, new }`, `HashCollision { id, existing, new }`, `UnknownType { name }`, `MissingTypeData { path, data }`. |

The standard types register the data of the traits they implement: all the opaque types get `ReflectDefault`,
`ReflectClone`, `ReflectPartialEq` and `ReflectDebug`, and all but `f32`/`f64` get `ReflectHash`; `Vec`, `Option`,
the maps and the sets get `ReflectDefault` only (their other traits depend on the item types, which the `Typed`
bounds do not know about). Arrays, tuples and `Box` get none. Use `add_type_data` for anything else.

On the derive: `#[reflect(Default, Clone, PartialEq, Debug, Hash)]` stores the matching data (the type has to
implement the traits); any other name that starts with an uppercase letter (`#[reflect(ReflectComponent)]`,
`#[reflect(my::ReflectThing)]`) is a type data of your own and gets `<Name as FromType<Type>>::from_type()`. Type
data attributes are rejected on generic types (the hook would need `T: Default` and the like); use
`TypeRegistry::add_type_data` for a generic instantiation. Every non-generic derived type also submits a
`TypeEntry` to `inventory` (re-exported as `reflection::__private`, not part of the API).

Registry-specific decisions:

* **Names.** The path and every alias of a type go into one name table; a second *different* type that wants a
  name already in the table is a `NameConflict` (`existing` and `new` are the paths of the two types, so two types
  with the same path show the same text twice). Two paths with the same 64-bit hash are a `HashCollision`; it is
  checked but cannot be produced by a test with realistic names.
* **All or nothing.** `register` first collects the type and everything it contains that is not registered yet,
  checks all of them against the registry and against each other, and only then inserts them.
* **Deterministic inventory.** `from_inventory` sorts the submitted types by path before registering them, so the
  same program reports the same conflict every time (the linker's order is not stable).
* **Aliases resolve in `get_by_path`, `create_default` and `from_reflect` of the registry, and in `field`/`field_mut` of
  derived types** (so in paths and `apply` too); the descriptions' own lookups use the real name.
* **Type data is registered, not discovered.** A type is only as capable as its registration says; nothing
  checks that `#[reflect(Clone)]` was written for a type that is `Clone` except the compiler (it would not
  compile otherwise).

## Paths, apply, clone, equality, dynamic values (R4)

| Item | Purpose |
|:-----|:--------|
| `ReflectPath`, `PathSegment` | A parsed path: `stats.health`, `items[2].0`, `pos.1`, `boss.0.0.name`. `.name` is a field by name (structs and struct variants), `.0` a field by position (tuples, tuple structs, tuple variants; an `Option<T>` or `Box<T>` is `.0`), `[2]` an item of a list or array. No syntax for map entries. `parse` reports the byte position of a problem (`InvalidPath`); `reflect` and `reflect_mut` walk a value and say which segment did not fit (`PathMismatch { segment, reason }`). |
| `GetPath` | Implemented for every `Reflect` type, concrete or `dyn Reflect`: `reflect_path`, `reflect_path_mut`, `get_path::<T>`, `get_path_mut::<T>`, `set_path`. |
| `Reflect::apply(&dyn Reflect)` | Structural copy. Structs, tuples, tuple structs and enums of the same variant field by field (fields the source lacks stay, fields the target lacks are ignored); lists are resized to the source length; arrays must have the same length (`LengthMismatch`); maps and sets gain what they lack but **lose nothing**; opaque values are replaced by a clone; an enum with another variant is rebuilt with `try_replace`. The source may be a value of the same type or a dynamic value, so a patch can be a `DynamicStruct` with only the fields to change. Different kinds are a `KindMismatch`. An error can leave the value partly changed. |
| `Reflect::try_replace(&dyn Reflect)` | Replaces the whole value with `Typed::from_reflect(value)`; written by `impl_reflect_boilerplate!`. Restores a snapshot exactly, maps and sets included. |
| `Reflect::clone_value()` | Deep structural clone made of dynamic values (structs become `DynamicStruct`, lists `DynamicList`, and so on, a `Box<T>` a `DynamicTupleStruct`); opaque values are cloned as they are. Each dynamic value remembers the type it was cloned from (`represented_type_info()`). |
| `Reflect::reflect_eq(&dyn Reflect)` | Structural equality, `Option<bool>`: `Some(false)` at the first difference, `None` if an opaque value inside cannot be compared and no difference was found. The type is not compared (a struct equals its dynamic clone, and `A { x: 1 }` equals `B { x: 1 }`). NaN is not equal to itself. Map keys and set items are looked up through the other container, so a `HashMap` with composite keys does not equal its dynamic clone. |
| `Reflect::opaque_clone`, `opaque_eq` | What `clone_value`, `apply` and `reflect_eq` use on opaque leaves. The standard opaque types implement them; a hand-written opaque type that is `Clone`/`PartialEq` should too (the default is `None`: not cloneable, not comparable). |
| `Typed::from_reflect(&dyn Reflect) -> Option<Self>` | Builds a concrete value from a reflected value of the same shape (this is the `FromReflect` of the plan, a provided method of `Typed` rather than a separate trait: containers use it without extra bounds). The derive and the standard types implement it; the default returns `None`. Lists, maps, sets, `Option` and `Box` convert their items with it, which is how dynamic values become concrete values inside `push`, `insert` and `apply`. |
| `ReflectFromReflect` | Type data with the same function, in every `TypeRegistration`; `TypeRegistry::from_reflect(name, &value)` builds a value by type name (`Unsupported` if the type cannot be built). |
| `DynamicStruct`, `DynamicTupleStruct`, `DynamicTuple`, `DynamicEnum` (+ `DynamicVariant`), `DynamicList`, `DynamicArray`, `DynamicMap`, `DynamicSet` | Values built at run time. They implement the kind traits, so everything above works on them. Their `TypeInfo` is an opaque `"DynamicStruct"` and so on (what they look like is only known at run time); `kind()` and `reflect_ref()` say what they behave like. Maps and sets find keys with `reflect_eq`. |
| `ReflectError` | New variants `InvalidPath`, `PathMismatch`, `KindMismatch`, `LengthMismatch`, `Unsupported { operation, path }`. |

On the derive: it writes `Typed::from_reflect` unless the type says `#[reflect(no_from_reflect)]`. Fields are built with
`from_reflect` of their types and looked up by name or alias (tuple fields and tuple variants by position), an enum
picks its variant by name or alias, skipped fields get `Default::default()`, and `#[reflect(default)]` on a field uses
`Default::default()` when the source has no such field (a field that is there but of the wrong type is still an
error). A type with a skipped field that is not `Default` has to say `no_from_reflect`; such a type cannot be built
from a reflected value, and containers of it cannot grow from dynamic values.

R4-specific decisions:

* **Leaf operations are trait methods.** Cloning or comparing an opaque value from a `&dyn Reflect` needs the type's
  `Clone`/`PartialEq`, which the object-safe trait cannot see, so `opaque_clone` and `opaque_eq` are provided methods
  with a `None` default and the standard types override them. This is why `apply`, `clone_value` and `reflect_eq` need
  no registry.
* **`from_reflect` lives on `Typed`.** A separate `FromReflect` trait would have needed `T: FromReflect` bounds on
  every container implementation and on every derived generic type; a provided method keeps all of R1 to R3 as it was.
* **Maps and sets are patched, not mirrored.** `apply` adds and updates entries but never removes them, so a patch
  can leave a map alone. Use `try_replace` with a snapshot for an exact restore.
* **Dynamic names are interned.** The kind traits return `&'static str` names, so `DynamicStruct` and `DynamicEnum`
  intern the names given to them (leaked once per distinct name). Do not feed untrusted, unbounded names to them; a
  deserializer should take names from the type's description.
* **Dynamic enums cannot switch variant themselves.** `try_replace` on a dynamic value reports `Unsupported`.

## Metadata and the visitor (R5)

| Item | Purpose |
|:-----|:--------|
| `Metadata` | What a tool needs to know about a type, a field or a variant besides its shape: `tooltip()`, `category()`, `range()`, `is_readonly()`, `is_hidden()`, `is_transient()`, `has_default()`, `docs()` and typed custom attributes (`custom::<T>()`, `has_custom::<T>()`). Read it from the description: `TypeInfo::metadata()` (and the `metadata()` of each `*Info`), `FieldInfo::metadata()`, `VariantInfo::metadata()`. Empty for the standard types and for anything without attributes. Cheap to clone (custom values are shared), built by hand with `with_*` methods. |
| `ValueRange` | An inclusive `f64` range with `contains` and a `clamp` that never panics (the bounds are not checked: `min <= max` is expected). |
| `visit`, `ReflectVisitor`, `VisitControl`, `VisitNode`, `VisitSlot` | A depth-first walk over any `&dyn Reflect`. The visitor gets `enter` (before the children) and `leave` (after them) for the root, every field of a struct, tuple, tuple struct or of the **active** variant of an enum, every item of a list, array or set, and every key and value of a map. `VisitNode` says where the walk is: `path()`, `depth()`, `slot()` (`Root`, `Field { name, index, info }`, `Item`, `MapKey`, `MapValue { index, key }`), `field()` and `metadata()` of the field the value is in. `enter` answers `Continue`, `SkipChildren` (`leave` is still called) or `Stop` (ends the walk successfully, no more calls); an `Err` ends it and is returned by `visit`. |

The attributes of the derive (everything is read-only information; see the decisions below):

| Attribute | On | Meaning |
|:----------|:---|:--------|
| `tooltip = "..."`, `category = "..."` | type, field, variant | Texts for editors; not empty, once each. |
| `hidden` | type, field, variant | An editor does not show it. |
| `readonly` | type, field | An editor shows it but does not let it be changed. |
| `transient` | type, field | A serializer does not write it. Unlike `skip`, the field stays in the description and reachable. |
| `range(min = .., max = ..)` | field | Numbers that convert into `f64` (`0`, `0.5`, `-3_i16`). |
| `attr = <expression>` | type, field, variant | A typed custom attribute: any `Send + Sync + 'static` value, for example `attr = Slider { step: 0.5 }`. Repeatable; one value per type, a later value of the same type replaces the earlier one. |
| `default` (R4) | field | Now also recorded as `has_default()`, so a serializer knows the field may be missing in data. |
| doc comments | type, field, variant | Captured as `docs()` only with the `docs` cargo feature of `reflection` (it turns on `reflection-macros/docs`); otherwise no text is kept in the binary. |

R5-specific decisions:

* **Metadata is information, not enforcement.** `apply`, `path_set`, `reflect_mut` and `from_reflect` ignore it: a
  `readonly` field can be written through reflection, a value outside `range` can be stored. Editors and serializers
  decide what to do with it (for example `ValueRange::clamp`).
* **One `Metadata` type for everything.** A flag that makes no sense in a place (`range` on a type, `readonly` on a
  variant) is a compile error of the derive, not silently stored.
* **Custom attributes are ordinary values.** They are held as `Arc<dyn Any + Send + Sync>` and found by `TypeId`, so
  there is no registration step and no `Reflect` bound. They are not reflected themselves.
* **`range` is written as `range(min = .., max = ..)`** and not as `min..=max`, so that the derive needs nothing
  from `syn` but the attribute parser; the values go through `Into<f64>`, so `f64`, `f32`, small integers and
  `i32` literals work, `i64` and larger integers do not.
* **The visitor is read-only and not fallible by itself.** The error type belongs to the visitor (`Infallible` for
  none). A mutable walk (editors that change values on the way) is not part of R5.
* **Paths in `VisitNode::path()` are for people.** Structs, tuples, tuple structs, enums and lists use the text form
  of `ReflectPath`; map entries and set items have no `ReflectPath` form and read `[key 0]`, `[value 0]` and
  `[item 0]` (position in iteration order).
* **Field descriptions come from the represented type.** A dynamic value that stands for a type (a `clone_value`
  snapshot) gives its fields' metadata too; a dynamic value that stands for nothing (a hand-made `DynamicStruct`)
  has `field()` equal to `None`.

## Example

```rust
use reflection::{Reflect, ReflectMut, Typed};

let mut numbers: Vec<u32> = vec![1, 2];
assert_eq!(Vec::<u32>::static_type_info().path(), "Vec<u32>");

let value: &mut dyn Reflect = &mut numbers;
match value.reflect_mut() {
    ReflectMut::List(list) => list.push(Box::new(3u32)).unwrap(),
    _ => unreachable!(),
}
assert_eq!(numbers, [1, 2, 3]);
```

`crates/reflection/tests/hand_written.rs` shows what a hand-written implementation looks like (a `Typed` impl with a
`TypeInfoCell`, a `Reflect` impl that starts with `impl_reflect_boilerplate!()`, and a `Struct` impl) for a struct
with five fields and a recursive `Node`; `tests/derive.rs` covers the derive: structs, tuple and unit structs, enums,
generics, paths, aliases and skipped fields, recursive types, containers of derived types; `tests/path.rs`,
`tests/apply.rs` and `tests/clone_eq.rs` cover R4; `tests/metadata.rs` and `tests/visitor.rs` cover R5; `tests/registry.rs` and
`tests/registry_conflicts.rs` cover the registry, type data and name conflicts (every integration test file is its own
program, so each has its own `inventory`).

## Planned

| Milestone | Contents |
|:----------|:---------|
| R1 | The core model. Implemented. |
| R2 | `reflection-macros` (`engine/rust/macros`): `#[derive(Reflect)]` for structs, tuple structs, unit structs, enums and generics; `skip`, `name`, `alias`, `rename`. Implemented. |
| R3 | `TypeRegistry` (lookup by key, name and alias; collision errors), automatic collection with `inventory`, `TypeData` (`ReflectDefault`, `ReflectClone`, `ReflectPartialEq`, `ReflectDebug`, `ReflectHash`). Implemented. |
| R4 | Paths (`a.b[2].c`), `apply`, `clone_value`, `reflect_eq`, dynamic values and `FromReflect`. Implemented. |
| R5 | Field and type metadata (range, tooltip, category, readonly, hidden, transient, custom attributes, optional doc comments) and a visitor. Implemented. |
| R6 | Engine types behind features (`math`, `ccore` ids, `Version`, `Handle`), `Arc` and `Cow`, edge cases. |
| R7 | `registry.validate()`, diagnostics (optional `logging` feature), text and JSON schema export, an optional FFI surface for the editor. |
| R8 | Benchmarks, remaining documentation, Miri. |
