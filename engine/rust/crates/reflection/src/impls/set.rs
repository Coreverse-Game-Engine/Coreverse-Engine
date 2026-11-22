//! `HashSet<T>` and `BTreeSet<T>`.

use std::collections::{BTreeSet, HashSet};
use std::hash::Hash;

use crate::cell::GenericTypeInfoCell;
use crate::error::ReflectError;
use crate::info::{SetInfo, TypeInfo, TypeRef};
use crate::key::TypeKey;
use crate::reflect::{Reflect, ReflectIter, ReflectMut, ReflectRef, Set, Typed};
use crate::registry::TypeRegistration;
use crate::type_data::{FromType, ReflectDefault};

impl<T: Typed + Hash + Eq> Typed for HashSet<T> {
    fn static_type_info() -> &'static TypeInfo {
        static CELL: GenericTypeInfoCell = GenericTypeInfoCell::new();
        CELL.get_or_insert::<Self>(|| {
            let key = TypeKey::from_owned(&format!("HashSet<{}>", T::static_type_info().path()));
            TypeInfo::Set(SetInfo::new::<Self>(key, TypeRef::of::<T>()))
        })
    }

    fn register_type_data(registration: &mut TypeRegistration) {
        registration.insert_data(<ReflectDefault as FromType<Self>>::from_type());
    }
}

impl<T: Typed + Hash + Eq> Reflect for HashSet<T> {
    crate::impl_reflect_boilerplate!();

    fn reflect_ref(&self) -> ReflectRef<'_> {
        ReflectRef::Set(self)
    }

    fn reflect_mut(&mut self) -> ReflectMut<'_> {
        ReflectMut::Set(self)
    }
}

impl<T: Typed + Hash + Eq> Set for HashSet<T> {
    fn len(&self) -> usize {
        HashSet::len(self)
    }

    fn contains(&self, value: &dyn Reflect) -> bool {
        match value.downcast_ref::<T>() {
            Some(value) => HashSet::contains(self, value),
            None => false,
        }
    }

    fn insert(&mut self, value: Box<dyn Reflect>) -> Result<bool, ReflectError> {
        let value = value.take::<T>()?;
        Ok(HashSet::insert(self, value))
    }

    fn remove(&mut self, value: &dyn Reflect) -> bool {
        match value.downcast_ref::<T>() {
            Some(value) => HashSet::remove(self, value),
            None => false,
        }
    }

    fn clear(&mut self) {
        HashSet::clear(self);
    }

    fn iter(&self) -> ReflectIter<'_, &dyn Reflect> {
        Box::new(HashSet::iter(self).map(|item| item as &dyn Reflect))
    }
}

impl<T: Typed + Ord> Typed for BTreeSet<T> {
    fn static_type_info() -> &'static TypeInfo {
        static CELL: GenericTypeInfoCell = GenericTypeInfoCell::new();
        CELL.get_or_insert::<Self>(|| {
            let key = TypeKey::from_owned(&format!("BTreeSet<{}>", T::static_type_info().path()));
            TypeInfo::Set(SetInfo::new::<Self>(key, TypeRef::of::<T>()))
        })
    }

    fn register_type_data(registration: &mut TypeRegistration) {
        registration.insert_data(<ReflectDefault as FromType<Self>>::from_type());
    }
}

impl<T: Typed + Ord> Reflect for BTreeSet<T> {
    crate::impl_reflect_boilerplate!();

    fn reflect_ref(&self) -> ReflectRef<'_> {
        ReflectRef::Set(self)
    }

    fn reflect_mut(&mut self) -> ReflectMut<'_> {
        ReflectMut::Set(self)
    }
}

impl<T: Typed + Ord> Set for BTreeSet<T> {
    fn len(&self) -> usize {
        BTreeSet::len(self)
    }

    fn contains(&self, value: &dyn Reflect) -> bool {
        match value.downcast_ref::<T>() {
            Some(value) => BTreeSet::contains(self, value),
            None => false,
        }
    }

    fn insert(&mut self, value: Box<dyn Reflect>) -> Result<bool, ReflectError> {
        let value = value.take::<T>()?;
        Ok(BTreeSet::insert(self, value))
    }

    fn remove(&mut self, value: &dyn Reflect) -> bool {
        match value.downcast_ref::<T>() {
            Some(value) => BTreeSet::remove(self, value),
            None => false,
        }
    }

    fn clear(&mut self) {
        BTreeSet::clear(self);
    }

    fn iter(&self) -> ReflectIter<'_, &dyn Reflect> {
        Box::new(BTreeSet::iter(self).map(|item| item as &dyn Reflect))
    }
}
