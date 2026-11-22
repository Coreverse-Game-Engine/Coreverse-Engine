# Reflection

`engine/rust/crates/reflection` is the engine's runtime type information: what a type is, what it is made of, and
a way to read and change values without knowing their type at compile time. It is being built in milestones; this
page describes what exists (R1, the core model, R2, `#[derive(Reflect)]`, and R3, the type registry and type data)
and what is planned. The crate uses `std`, `utilities` (the name
hash and poison-tolerant locks) and `ccore` (`ReflectError` converts into `CoreError`), and contains no `unsafe`
code.

> **Verification status.** R1, R2 and R3 were written in a sandbox without a Rust toolchain. They have not been
> compiled, linted or tested yet. Run `cargo fmt --all`, `cargo clippy -p reflection -p reflection-macros
> --all-targets`, `cargo test -p reflection -p reflection-macros` and `cargo doc -p reflection --no-deps` before
> merging.

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
  crate, which will walk values through the visitor of R5.
* **`Debug`, `Clone`, `PartialEq`, `Default` and `Hash` of reflected values** go through the type data of R3, not
  through the `Reflect` trait; structural versions (`reflect_eq`, `clone_value`) come with R4.

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
  (`StructInfo::field`), which use the real name. A derive does not generate `Default`: skipped fields are
  restored by `FromReflect` in R4, which will need `Default` or a `#[reflect(default)]` for them.
* Changing the variant of an enum in place is still not possible; `set` replaces the whole value (R4 adds dynamic
  values).

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
* **Aliases resolve in `get_by_path` and `create_default`**, nowhere else yet (paths and `apply` come in R4).
* **Type data is registered, not discovered.** A type is only as capable as its registration says; nothing
  checks that `#[reflect(Clone)]` was written for a type that is `Clone` except the compiler (it would not
  compile otherwise).

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
generics, paths, aliases and skipped fields, recursive types, containers of derived types; `tests/registry.rs` and
`tests/registry_conflicts.rs` cover the registry, type data and name conflicts (every integration test file is its own
program, so each has its own `inventory`).

## Planned

| Milestone | Contents |
|:----------|:---------|
| R1 | The core model. Implemented. |
| R2 | `reflection-macros` (`engine/rust/macros`): `#[derive(Reflect)]` for structs, tuple structs, unit structs, enums and generics; `skip`, `name`, `alias`, `rename`. Implemented. |
| R3 | `TypeRegistry` (lookup by key, name and alias; collision errors), automatic collection with `inventory`, `TypeData` (`ReflectDefault`, `ReflectClone`, `ReflectPartialEq`, `ReflectDebug`, `ReflectHash`). Implemented. |
| R4 | Paths (`a.b[2].c`), `apply`, `clone_value`, `reflect_eq`, dynamic values and `FromReflect`. |
| R5 | Field and type metadata (range, tooltip, category, readonly, hidden, transient, custom attributes) and a visitor. |
| R6 | Engine types behind features (`math`, `ccore` ids, `Version`, `Handle`), `Arc` and `Cow`, edge cases. |
| R7 | `registry.validate()`, diagnostics (optional `logging` feature), text and JSON schema export, an optional FFI surface for the editor. |
| R8 | Benchmarks, remaining documentation, Miri. |
