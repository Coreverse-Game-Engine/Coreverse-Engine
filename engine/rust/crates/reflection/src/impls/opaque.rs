//! Numbers, `bool`, `char`, `String` and `()`: values reflection does not look into.
//!
//! Each registers the type data of the standard traits it implements: `ReflectDefault`, `ReflectClone`,
//! `ReflectPartialEq` and `ReflectDebug` for all, `ReflectHash` for all but the floats.

use crate::cell::TypeInfoCell;
use crate::info::{OpaqueInfo, TypeInfo};
use crate::key::TypeKey;
use crate::reflect::{Reflect, ReflectMut, ReflectRef, Typed};
use crate::registry::TypeRegistration;
use crate::type_data::{
    FromType, ReflectClone, ReflectDebug, ReflectDefault, ReflectHash, ReflectPartialEq,
};

macro_rules! impl_opaque {
    ($($ty:ty => $path:literal : [$($data:ident),*]),+ $(,)?) => {
        $(
            impl Typed for $ty {
                fn static_type_info() -> &'static TypeInfo {
                    static CELL: TypeInfoCell = TypeInfoCell::new();
                    CELL.get_or_init(|| {
                        TypeInfo::Opaque(OpaqueInfo::new::<$ty>(TypeKey::new($path)))
                    })
                }

                #[allow(unused_variables)]
                fn register_type_data(registration: &mut TypeRegistration) {
                    $(
                        registration.insert_data(<$data as FromType<$ty>>::from_type());
                    )*
                }
            }

            impl Reflect for $ty {
                crate::impl_reflect_boilerplate!();

                fn reflect_ref(&self) -> ReflectRef<'_> {
                    ReflectRef::Opaque(self)
                }

                fn reflect_mut(&mut self) -> ReflectMut<'_> {
                    ReflectMut::Opaque(self)
                }
            }
        )+
    };
}

impl_opaque! {
    () => "()": [ReflectDefault, ReflectClone, ReflectPartialEq, ReflectDebug, ReflectHash],
    bool => "bool": [ReflectDefault, ReflectClone, ReflectPartialEq, ReflectDebug, ReflectHash],
    char => "char": [ReflectDefault, ReflectClone, ReflectPartialEq, ReflectDebug, ReflectHash],
    u8 => "u8": [ReflectDefault, ReflectClone, ReflectPartialEq, ReflectDebug, ReflectHash],
    u16 => "u16": [ReflectDefault, ReflectClone, ReflectPartialEq, ReflectDebug, ReflectHash],
    u32 => "u32": [ReflectDefault, ReflectClone, ReflectPartialEq, ReflectDebug, ReflectHash],
    u64 => "u64": [ReflectDefault, ReflectClone, ReflectPartialEq, ReflectDebug, ReflectHash],
    u128 => "u128": [ReflectDefault, ReflectClone, ReflectPartialEq, ReflectDebug, ReflectHash],
    usize => "usize": [ReflectDefault, ReflectClone, ReflectPartialEq, ReflectDebug, ReflectHash],
    i8 => "i8": [ReflectDefault, ReflectClone, ReflectPartialEq, ReflectDebug, ReflectHash],
    i16 => "i16": [ReflectDefault, ReflectClone, ReflectPartialEq, ReflectDebug, ReflectHash],
    i32 => "i32": [ReflectDefault, ReflectClone, ReflectPartialEq, ReflectDebug, ReflectHash],
    i64 => "i64": [ReflectDefault, ReflectClone, ReflectPartialEq, ReflectDebug, ReflectHash],
    i128 => "i128": [ReflectDefault, ReflectClone, ReflectPartialEq, ReflectDebug, ReflectHash],
    isize => "isize": [ReflectDefault, ReflectClone, ReflectPartialEq, ReflectDebug, ReflectHash],
    f32 => "f32": [ReflectDefault, ReflectClone, ReflectPartialEq, ReflectDebug],
    f64 => "f64": [ReflectDefault, ReflectClone, ReflectPartialEq, ReflectDebug],
    String => "String": [ReflectDefault, ReflectClone, ReflectPartialEq, ReflectDebug, ReflectHash],
}
