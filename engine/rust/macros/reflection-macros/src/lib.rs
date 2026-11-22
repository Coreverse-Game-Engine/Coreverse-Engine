//! The `#[derive(Reflect)]` macro of the `reflection` crate.
//!
//! Kept in `engine/rust/macros/` next to `math-macros` and `vfs-macros`. Use it through the `reflection`
//! crate (`use reflection::Reflect;`), which re-exports it next to the trait of the same name; the generated
//! code names `::reflection::...`, so the crate that uses the derive must depend on `reflection`.

use proc_macro::TokenStream;
use syn::{DeriveInput, parse_macro_input};

mod attrs;
mod expand;

/// Derives `Typed`, `Reflect` and the trait of the type's kind (`Struct`, `TupleStruct` or `Enum`).
///
/// * A struct with named fields (and a unit struct, which has no fields) is reflected as a `Struct`, a tuple
///   struct as a `TupleStruct`, an enum as an `Enum` (unit, tuple and struct variants).
/// * Every field type has to be `Typed` (and so `Reflect`); the whole type has to be `Send + Sync`.
/// * Generic types are supported: every type parameter gets a `Typed` bound, and the path of an instantiation
///   contains the paths of its arguments (`game::Pair<u32,String>`). Lifetime parameters are an error
///   (reflected values are `'static`).
/// * The stable path of the type (its `TypeKey`) defaults to `module_path!()` plus the type name, for example
///   `game::player::Player`. Moving the type to another module changes the path; `name` pins it.
///
/// # Attributes
///
/// On the type, `#[reflect(...)]`:
///
/// * `name = "game::Player"`: the path of the type (for a generic type, the part before the arguments).
/// * `alias = "game::OldPlayer"`: another path the type was known by; repeat it for several. Not allowed on
///   generic types.
/// * `Default`, `Clone`, `PartialEq`, `Debug`, `Hash`: store `ReflectDefault`, `ReflectClone`,
///   `ReflectPartialEq`, `ReflectDebug` or `ReflectHash` in the type's registration (the type has to
///   implement the trait). Any other name that starts with an uppercase letter is a type data of your own: the
///   macro calls `<Name as FromType<Type>>::from_type()`. Not allowed on generic types.
///
/// Every non-generic type also submits a `TypeEntry` to `inventory`, which `TypeRegistry::from_inventory`
/// collects.
///
/// On a field:
///
/// * `skip`: the field is not reflected (not in the description, not reachable). It does not need to be
///   `Reflect`.
/// * `rename = "hp"`: the reflected name of a named field.
/// * `alias = "health"`: another name of a named field; repeat it for several. `field` and `field_mut` accept
///   aliases, `name_at` and the description give the name.
///
/// On an enum variant:
///
/// * `rename = "Idle"` and `alias = "Waiting"`, like on a field.
///
/// ```ignore
/// use reflection::{Reflect, ReflectRef, Typed};
///
/// #[derive(Reflect)]
/// struct Player {
///     name: String,
///     #[reflect(rename = "hp", alias = "health")]
///     health: u32,
///     #[reflect(skip)]
///     cache: Vec<u8>,
/// }
///
/// let player = Player { name: "Ada".into(), health: 10, cache: Vec::new() };
/// let ReflectRef::Struct(view) = Reflect::reflect_ref(&player) else { unreachable!() };
/// assert_eq!(view.field_len(), 2);
/// assert_eq!(view.name_at(1), Some("hp"));
/// assert_eq!(view.field("health").unwrap().downcast_ref::<u32>(), Some(&10));
/// assert!(Player::static_type_info().path().ends_with("Player"));
/// ```
#[proc_macro_derive(Reflect, attributes(reflect))]
pub fn derive_reflect(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand::derive(&input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}
