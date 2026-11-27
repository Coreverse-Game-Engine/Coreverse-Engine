//! Tuples with one to eight fields.

use crate::cell::GenericTypeInfoCell;
use crate::info::{FieldInfo, TupleInfo, TypeInfo, TypeRef};
use crate::key::TypeKey;
use crate::reflect::{Reflect, ReflectMut, ReflectRef, Tuple, Typed};

macro_rules! impl_tuple {
    ($($name:ident $index:tt),+) => {
        impl<$($name: Typed),+> Typed for ($($name,)+) {
            fn static_type_info() -> &'static TypeInfo {
                static CELL: GenericTypeInfoCell = GenericTypeInfoCell::new();
                CELL.get_or_insert::<Self>(|| {
                    let parts = [$(<$name as Typed>::static_type_info().path()),+];
                    let key = TypeKey::from_owned(&format!("({})", parts.join(",")));
                    TypeInfo::Tuple(TupleInfo::new::<Self>(
                        key,
                        vec![$(FieldInfo::unnamed($index, TypeRef::of::<$name>())),+],
                    ))
                })
            }

            fn from_reflect(value: &dyn Reflect) -> Option<Self> {
                let ReflectRef::Tuple(view) = value.reflect_ref() else {
                    return None;
                };
                if view.field_len() != [$($index),+].len() {
                    return None;
                }
                Some(($(<$name as Typed>::from_reflect(view.field($index)?)?,)+))
            }
        }

        impl<$($name: Typed),+> Reflect for ($($name,)+) {
            crate::impl_reflect_boilerplate!();

            fn reflect_ref(&self) -> ReflectRef<'_> {
                ReflectRef::Tuple(self)
            }

            fn reflect_mut(&mut self) -> ReflectMut<'_> {
                ReflectMut::Tuple(self)
            }
        }

        impl<$($name: Typed),+> Tuple for ($($name,)+) {
            fn field_len(&self) -> usize {
                [$($index),+].len()
            }

            fn field(&self, index: usize) -> Option<&dyn Reflect> {
                match index {
                    $($index => Some(&self.$index as &dyn Reflect),)+
                    _ => None,
                }
            }

            fn field_mut(&mut self, index: usize) -> Option<&mut dyn Reflect> {
                match index {
                    $($index => Some(&mut self.$index as &mut dyn Reflect),)+
                    _ => None,
                }
            }
        }
    };
}

impl_tuple!(A 0);
impl_tuple!(A 0, B 1);
impl_tuple!(A 0, B 1, C 2);
impl_tuple!(A 0, B 1, C 2, D 3);
impl_tuple!(A 0, B 1, C 2, D 3, E 4);
impl_tuple!(A 0, B 1, C 2, D 3, E 4, F 5);
impl_tuple!(A 0, B 1, C 2, D 3, E 4, F 5, G 6);
impl_tuple!(A 0, B 1, C 2, D 3, E 4, F 5, G 6, H 7);
