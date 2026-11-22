# Reflection

`engine/rust/crates/reflection` is the engine's runtime type information: what a type is, what it is made of, and
a way to read and change values without knowing their type at compile time. It is being built in milestones; this
page describes what exists (R1, the core model, and R2, `#[derive(Reflect)]`) and what is planned. The crate uses `std`, `utilities` (the name
hash and poison-tolerant locks) and `ccore` (`ReflectError` converts into `CoreError`), and contains no `unsafe`
code.

> **Verification status.** R1 and R2 were written in a sandbox without a Rust toolchain. They have not been
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
* **No `Debug` of reflected values yet.** `ReflectDebug`, `ReflectClone`, `ReflectPartialEq` and `ReflectDefault`
  arrive with the type data of R3.

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
generics, paths, aliases and skipped fields, recursive types, containers of derived types.

## Planned

| Milestone | Contents |
|:----------|:---------|
| R1 | The core model. Implemented. |
| R2 | `reflection-macros` (`engine/rust/macros`): `#[derive(Reflect)]` for structs, tuple structs, unit structs, enums and generics; `skip`, `name`, `alias`, `rename`. Implemented. |
| R3 | `TypeRegistry` (lookup by key, name and alias; collision errors), automatic collection with `inventory`, `TypeData` (`ReflectDefault`, `ReflectClone`, `ReflectPartialEq`, `ReflectDebug`, `ReflectHash`). |
| R4 | Paths (`a.b[2].c`), `apply`, `clone_value`, `reflect_eq`, dynamic values and `FromReflect`. |
| R5 | Field and type metadata (range, tooltip, category, readonly, hidden, transient, custom attributes) and a visitor. |
| R6 | Engine types behind features (`math`, `ccore` ids, `Version`, `Handle`), `Arc` and `Cow`, edge cases. |
| R7 | `registry.validate()`, diagnostics (optional `logging` feature), text and JSON schema export, an optional FFI surface for the editor. |
| R8 | Benchmarks, remaining documentation, Miri. |
