//! `Box<T>`: a tuple struct with one field, the boxed value.
//!
//! Reflection has no "pointer" kind. A box shows up as a tuple struct called `Box<T>` whose only field is
//! the value inside; code that does not care about the indirection (a serializer, an inspector) steps
//! through it. It exists mainly so that recursive types (`Option<Box<Node>>`) can be reflected.

use crate::cell::GenericTypeInfoCell;
use crate::info::{FieldInfo, TupleStructInfo, TypeInfo, TypeRef};
use crate::key::TypeKey;
use crate::reflect::{Reflect, ReflectMut, ReflectRef, TupleStruct, Typed};

impl<T: Typed> Typed for Box<T> {
    fn static_type_info() -> &'static TypeInfo {
        static CELL: GenericTypeInfoCell = GenericTypeInfoCell::new();
        CELL.get_or_insert::<Self>(|| {
            let key = TypeKey::from_owned(&format!("Box<{}>", T::static_type_info().path()));
            TypeInfo::TupleStruct(TupleStructInfo::new::<Self>(
                key,
                vec![FieldInfo::unnamed(0, TypeRef::of::<T>())],
            ))
        })
    }
}

impl<T: Typed> Reflect for Box<T> {
    crate::impl_reflect_boilerplate!();

    fn reflect_ref(&self) -> ReflectRef<'_> {
        ReflectRef::TupleStruct(self)
    }

    fn reflect_mut(&mut self) -> ReflectMut<'_> {
        ReflectMut::TupleStruct(self)
    }
}

impl<T: Typed> TupleStruct for Box<T> {
    fn field_len(&self) -> usize {
        1
    }

    fn field(&self, index: usize) -> Option<&dyn Reflect> {
        match index {
            0 => Some(&**self as &dyn Reflect),
            _ => None,
        }
    }

    fn field_mut(&mut self, index: usize) -> Option<&mut dyn Reflect> {
        match index {
            0 => Some(&mut **self as &mut dyn Reflect),
            _ => None,
        }
    }
}
