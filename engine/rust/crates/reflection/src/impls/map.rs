//! `HashMap<K, V>` and `BTreeMap<K, V>`.

use std::collections::{BTreeMap, HashMap};
use std::hash::Hash;

use crate::cell::GenericTypeInfoCell;
use crate::error::ReflectError;
use crate::info::{MapInfo, TypeInfo, TypeRef};
use crate::key::TypeKey;
use crate::reflect::{Map, Reflect, ReflectIter, ReflectMut, ReflectRef, Typed};
use crate::registry::TypeRegistration;
use crate::type_data::{FromType, ReflectDefault};

impl<K: Typed + Hash + Eq, V: Typed> Typed for HashMap<K, V> {
    fn static_type_info() -> &'static TypeInfo {
        static CELL: GenericTypeInfoCell = GenericTypeInfoCell::new();
        CELL.get_or_insert::<Self>(|| {
            let key = TypeKey::from_owned(&format!(
                "HashMap<{},{}>",
                K::static_type_info().path(),
                V::static_type_info().path()
            ));
            TypeInfo::Map(MapInfo::new::<Self>(
                key,
                TypeRef::of::<K>(),
                TypeRef::of::<V>(),
            ))
        })
    }

    fn register_type_data(registration: &mut TypeRegistration) {
        registration.insert_data(<ReflectDefault as FromType<Self>>::from_type());
    }

    fn from_reflect(value: &dyn Reflect) -> Option<Self> {
        let ReflectRef::Map(view) = value.reflect_ref() else {
            return None;
        };
        let mut map: HashMap<K, V> = HashMap::new();
        for (key, item) in view.iter() {
            HashMap::insert(&mut map, K::from_reflect(key)?, V::from_reflect(item)?);
        }
        Some(map)
    }
}

impl<K: Typed + Hash + Eq, V: Typed> Reflect for HashMap<K, V> {
    crate::impl_reflect_boilerplate!();

    fn reflect_ref(&self) -> ReflectRef<'_> {
        ReflectRef::Map(self)
    }

    fn reflect_mut(&mut self) -> ReflectMut<'_> {
        ReflectMut::Map(self)
    }
}

impl<K: Typed + Hash + Eq, V: Typed> Map for HashMap<K, V> {
    fn len(&self) -> usize {
        HashMap::len(self)
    }

    fn get(&self, key: &dyn Reflect) -> Option<&dyn Reflect> {
        let key = key.downcast_ref::<K>()?;
        HashMap::get(self, key).map(|value| value as &dyn Reflect)
    }

    fn get_mut(&mut self, key: &dyn Reflect) -> Option<&mut dyn Reflect> {
        let key = key.downcast_ref::<K>()?;
        HashMap::get_mut(self, key).map(|value| value as &mut dyn Reflect)
    }

    fn insert(
        &mut self,
        key: Box<dyn Reflect>,
        value: Box<dyn Reflect>,
    ) -> Result<Option<Box<dyn Reflect>>, ReflectError> {
        let key = key.take_or_convert::<K>()?;
        let value = value.take_or_convert::<V>()?;
        Ok(HashMap::insert(self, key, value).map(|old| Box::new(old) as Box<dyn Reflect>))
    }

    fn remove(&mut self, key: &dyn Reflect) -> Option<Box<dyn Reflect>> {
        let key = key.downcast_ref::<K>()?;
        HashMap::remove(self, key).map(|old| Box::new(old) as Box<dyn Reflect>)
    }

    fn clear(&mut self) {
        HashMap::clear(self);
    }

    fn iter(&self) -> ReflectIter<'_, (&dyn Reflect, &dyn Reflect)> {
        Box::new(
            HashMap::iter(self).map(|(key, value)| (key as &dyn Reflect, value as &dyn Reflect)),
        )
    }
}

impl<K: Typed + Ord, V: Typed> Typed for BTreeMap<K, V> {
    fn static_type_info() -> &'static TypeInfo {
        static CELL: GenericTypeInfoCell = GenericTypeInfoCell::new();
        CELL.get_or_insert::<Self>(|| {
            let key = TypeKey::from_owned(&format!(
                "BTreeMap<{},{}>",
                K::static_type_info().path(),
                V::static_type_info().path()
            ));
            TypeInfo::Map(MapInfo::new::<Self>(
                key,
                TypeRef::of::<K>(),
                TypeRef::of::<V>(),
            ))
        })
    }

    fn register_type_data(registration: &mut TypeRegistration) {
        registration.insert_data(<ReflectDefault as FromType<Self>>::from_type());
    }

    fn from_reflect(value: &dyn Reflect) -> Option<Self> {
        let ReflectRef::Map(view) = value.reflect_ref() else {
            return None;
        };
        let mut map: BTreeMap<K, V> = BTreeMap::new();
        for (key, item) in view.iter() {
            BTreeMap::insert(&mut map, K::from_reflect(key)?, V::from_reflect(item)?);
        }
        Some(map)
    }
}

impl<K: Typed + Ord, V: Typed> Reflect for BTreeMap<K, V> {
    crate::impl_reflect_boilerplate!();

    fn reflect_ref(&self) -> ReflectRef<'_> {
        ReflectRef::Map(self)
    }

    fn reflect_mut(&mut self) -> ReflectMut<'_> {
        ReflectMut::Map(self)
    }
}

impl<K: Typed + Ord, V: Typed> Map for BTreeMap<K, V> {
    fn len(&self) -> usize {
        BTreeMap::len(self)
    }

    fn get(&self, key: &dyn Reflect) -> Option<&dyn Reflect> {
        let key = key.downcast_ref::<K>()?;
        BTreeMap::get(self, key).map(|value| value as &dyn Reflect)
    }

    fn get_mut(&mut self, key: &dyn Reflect) -> Option<&mut dyn Reflect> {
        let key = key.downcast_ref::<K>()?;
        BTreeMap::get_mut(self, key).map(|value| value as &mut dyn Reflect)
    }

    fn insert(
        &mut self,
        key: Box<dyn Reflect>,
        value: Box<dyn Reflect>,
    ) -> Result<Option<Box<dyn Reflect>>, ReflectError> {
        let key = key.take_or_convert::<K>()?;
        let value = value.take_or_convert::<V>()?;
        Ok(BTreeMap::insert(self, key, value).map(|old| Box::new(old) as Box<dyn Reflect>))
    }

    fn remove(&mut self, key: &dyn Reflect) -> Option<Box<dyn Reflect>> {
        let key = key.downcast_ref::<K>()?;
        BTreeMap::remove(self, key).map(|old| Box::new(old) as Box<dyn Reflect>)
    }

    fn clear(&mut self) {
        BTreeMap::clear(self);
    }

    fn iter(&self) -> ReflectIter<'_, (&dyn Reflect, &dyn Reflect)> {
        Box::new(
            BTreeMap::iter(self).map(|(key, value)| (key as &dyn Reflect, value as &dyn Reflect)),
        )
    }
}
