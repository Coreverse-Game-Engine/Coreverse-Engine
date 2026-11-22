//! `Option<T>`: an enum with the variants `None` and `Some(T)`.

use crate::cell::GenericTypeInfoCell;
use crate::info::{EnumInfo, FieldInfo, TypeInfo, TypeRef, VariantInfo, VariantKind};
use crate::key::TypeKey;
use crate::reflect::{Enum, Reflect, ReflectMut, ReflectRef, Typed};
use crate::registry::TypeRegistration;
use crate::type_data::{FromType, ReflectDefault};

impl<T: Typed> Typed for Option<T> {
    fn static_type_info() -> &'static TypeInfo {
        static CELL: GenericTypeInfoCell = GenericTypeInfoCell::new();
        CELL.get_or_insert::<Self>(|| {
            let key = TypeKey::from_owned(&format!("Option<{}>", T::static_type_info().path()));
            TypeInfo::Enum(EnumInfo::new::<Self>(
                key,
                vec![
                    VariantInfo::unit("None", 0),
                    VariantInfo::tuple("Some", 1, vec![FieldInfo::unnamed(0, TypeRef::of::<T>())]),
                ],
            ))
        })
    }

    fn register_type_data(registration: &mut TypeRegistration) {
        registration.insert_data(<ReflectDefault as FromType<Self>>::from_type());
    }
}

impl<T: Typed> Reflect for Option<T> {
    crate::impl_reflect_boilerplate!();

    fn reflect_ref(&self) -> ReflectRef<'_> {
        ReflectRef::Enum(self)
    }

    fn reflect_mut(&mut self) -> ReflectMut<'_> {
        ReflectMut::Enum(self)
    }
}

impl<T: Typed> Enum for Option<T> {
    fn variant_name(&self) -> &'static str {
        match self {
            None => "None",
            Some(_) => "Some",
        }
    }

    fn variant_index(&self) -> usize {
        match self {
            None => 0,
            Some(_) => 1,
        }
    }

    fn variant_kind(&self) -> VariantKind {
        match self {
            None => VariantKind::Unit,
            Some(_) => VariantKind::Tuple,
        }
    }

    fn field_len(&self) -> usize {
        match self {
            None => 0,
            Some(_) => 1,
        }
    }

    fn name_at(&self, _index: usize) -> Option<&'static str> {
        None
    }

    fn field(&self, _name: &str) -> Option<&dyn Reflect> {
        None
    }

    fn field_mut(&mut self, _name: &str) -> Option<&mut dyn Reflect> {
        None
    }

    fn field_at(&self, index: usize) -> Option<&dyn Reflect> {
        match self {
            Some(value) if index == 0 => Some(value as &dyn Reflect),
            _ => None,
        }
    }

    fn field_at_mut(&mut self, index: usize) -> Option<&mut dyn Reflect> {
        match self {
            Some(value) if index == 0 => Some(value as &mut dyn Reflect),
            _ => None,
        }
    }
}
