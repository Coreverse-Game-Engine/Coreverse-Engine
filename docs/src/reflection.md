# Reflection

`engine/rust/crates/reflection` is the engine's runtime type information: what a type is, what it is made of, and
a way to read and change values without knowing their type at compile time. It is being built in milestones; this
page describes what exists (R1, the core model) and what is planned. The crate uses `std`, `utilities` (the name
hash and poison-tolerant locks) and `ccore` (`ReflectError` converts into `CoreError`), and contains no `unsafe`
code.

> **Verification status.** R1 was written in a sandbox without a Rust toolchain. It has not been compiled, linted
> or tested yet. Run `cargo fmt --all`, `cargo clippy -p reflection --all-targets`, `cargo test -p reflection` and
> `cargo doc -p reflection --no-deps` before merging.

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
| `ReflectError` | `TypeMismatch { expected, actual }` and `IndexOutOfBounds { index, len }`; `Display`, `Error`, `From<ReflectError> for CoreError`; `#[non_exhaustive]`. |
| `impl_reflect_boilerplate!`, `TypeInfoCell`, `GenericTypeInfoCell` | What a hand-written implementation needs. The macro writes `type_info`, the six conversions and `set`; the cells hold the `'static` description (one per type, or one per instantiation of a generic type). The derive macro of R2 will use all three. |

Implemented for: the integer and float types, `bool`, `char`, `String`, `()` (opaque values); tuples of one to eight
fields; `[T; N]`; `Vec<T>`; `Option<T>` (an enum with the variants `None` and `Some`); `Box<T>`; `HashMap<K, V>` and
`BTreeMap<K, V>`; `HashSet<T>` and `BTreeSet<T>`. Every type argument must itself be `Typed`.

## Design decisions

* **Safe code only.** Fields are reached through `Any` downcasts and ordinary `&`/`&mut` borrows; there is no offset
  arithmetic. The price: two fields of one value cannot be borrowed mutably at the same time through reflection.
* **Own type keys.** The path of a built-in type is fixed (`"u32"`, `"Vec<T>"` with `T` replaced by the item type's
  path, ...); it does not follow the standard library's module layout. A user type chooses its own path (the derive
  macro of R2 will default to `crate::module::Name`, with `#[reflect(name = ...)]` and `alias` for renames).
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
* **No `Debug` of reflected values yet.** `ReflectDebug`, `ReflectClone`, `ReflectPartialEq` and `ReflectDefault`
  arrive with the type data of R3.

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

Until the derive macro (R2) exists, a struct is reflected by hand; `crates/reflection/tests/hand_written.rs` shows a
struct with five fields and a recursive `Node` (a `Typed` impl with a `TypeInfoCell`, a `Reflect` impl that starts
with `impl_reflect_boilerplate!()`, and a `Struct` impl).

## Planned

| Milestone | Contents |
|:----------|:---------|
| R1 | The core model (this page). |
| R2 | `reflection-macros` (`engine/rust/macros`): `#[derive(Reflect)]` for structs, tuple structs, unit structs, enums and generics; `skip`, `name`, `alias`, `rename`. |
| R3 | `TypeRegistry` (lookup by key, name and alias; collision errors), automatic collection with `inventory`, `TypeData` (`ReflectDefault`, `ReflectClone`, `ReflectPartialEq`, `ReflectDebug`, `ReflectHash`). |
| R4 | Paths (`a.b[2].c`), `apply`, `clone_value`, `reflect_eq`, dynamic values and `FromReflect`. |
| R5 | Field and type metadata (range, tooltip, category, readonly, hidden, transient, custom attributes) and a visitor. |
| R6 | Engine types behind features (`math`, `ccore` ids, `Version`, `Handle`), `Arc` and `Cow`, edge cases. |
| R7 | `registry.validate()`, diagnostics (optional `logging` feature), text and JSON schema export, an optional FFI surface for the editor. |
| R8 | Benchmarks, remaining documentation, Miri. |
