//! Numbers, `bool`, `char`, `String` and `()`: values reflection does not look into.

use crate::cell::TypeInfoCell;
use crate::info::{OpaqueInfo, TypeInfo};
use crate::key::TypeKey;
use crate::reflect::{Reflect, ReflectMut, ReflectRef, Typed};

macro_rules! impl_opaque {
    ($($ty:ty => $path:literal),+ $(,)?) => {
        $(
            impl Typed for $ty {
                fn static_type_info() -> &'static TypeInfo {
                    static CELL: TypeInfoCell = TypeInfoCell::new();
                    CELL.get_or_init(|| {
                        TypeInfo::Opaque(OpaqueInfo::new::<$ty>(TypeKey::new($path)))
                    })
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
    () => "()",
    bool => "bool",
    char => "char",
    u8 => "u8",
    u16 => "u16",
    u32 => "u32",
    u64 => "u64",
    u128 => "u128",
    usize => "usize",
    i8 => "i8",
    i16 => "i16",
    i32 => "i32",
    i64 => "i64",
    i128 => "i128",
    isize => "isize",
    f32 => "f32",
    f64 => "f64",
    String => "String",
}
