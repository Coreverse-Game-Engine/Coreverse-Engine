//! `Vec<T>` (a list) and `[T; N]` (an array).

use crate::cell::GenericTypeInfoCell;
use crate::error::ReflectError;
use crate::info::{ArrayInfo, ListInfo, TypeInfo, TypeRef};
use crate::key::TypeKey;
use crate::reflect::{Array, List, Reflect, ReflectIter, ReflectMut, ReflectRef, Typed};

impl<T: Typed> Typed for Vec<T> {
    fn static_type_info() -> &'static TypeInfo {
        static CELL: GenericTypeInfoCell = GenericTypeInfoCell::new();
        CELL.get_or_insert::<Self>(|| {
            let key = TypeKey::from_owned(&format!("Vec<{}>", T::static_type_info().path()));
            TypeInfo::List(ListInfo::new::<Self>(key, TypeRef::of::<T>()))
        })
    }
}

impl<T: Typed> Reflect for Vec<T> {
    crate::impl_reflect_boilerplate!();

    fn reflect_ref(&self) -> ReflectRef<'_> {
        ReflectRef::List(self)
    }

    fn reflect_mut(&mut self) -> ReflectMut<'_> {
        ReflectMut::List(self)
    }
}

impl<T: Typed> List for Vec<T> {
    fn len(&self) -> usize {
        self.as_slice().len()
    }

    fn get(&self, index: usize) -> Option<&dyn Reflect> {
        self.as_slice().get(index).map(|item| item as &dyn Reflect)
    }

    fn get_mut(&mut self, index: usize) -> Option<&mut dyn Reflect> {
        self.as_mut_slice()
            .get_mut(index)
            .map(|item| item as &mut dyn Reflect)
    }

    fn push(&mut self, value: Box<dyn Reflect>) -> Result<(), ReflectError> {
        let value = value.take::<T>()?;
        Vec::push(self, value);
        Ok(())
    }

    fn insert(&mut self, index: usize, value: Box<dyn Reflect>) -> Result<(), ReflectError> {
        let len = self.as_slice().len();
        if index > len {
            return Err(ReflectError::index_out_of_bounds(index, len));
        }
        let value = value.take::<T>()?;
        Vec::insert(self, index, value);
        Ok(())
    }

    fn pop(&mut self) -> Option<Box<dyn Reflect>> {
        Vec::pop(self).map(|item| Box::new(item) as Box<dyn Reflect>)
    }

    fn remove(&mut self, index: usize) -> Option<Box<dyn Reflect>> {
        if index >= self.as_slice().len() {
            return None;
        }
        Some(Box::new(Vec::remove(self, index)) as Box<dyn Reflect>)
    }

    fn clear(&mut self) {
        Vec::clear(self);
    }

    fn iter(&self) -> ReflectIter<'_, &dyn Reflect> {
        Box::new(self.as_slice().iter().map(|item| item as &dyn Reflect))
    }
}

impl<T: Typed, const N: usize> Typed for [T; N] {
    fn static_type_info() -> &'static TypeInfo {
        static CELL: GenericTypeInfoCell = GenericTypeInfoCell::new();
        CELL.get_or_insert::<Self>(|| {
            let key = TypeKey::from_owned(&format!("[{};{}]", T::static_type_info().path(), N));
            TypeInfo::Array(ArrayInfo::new::<Self>(key, TypeRef::of::<T>(), N))
        })
    }
}

impl<T: Typed, const N: usize> Reflect for [T; N] {
    crate::impl_reflect_boilerplate!();

    fn reflect_ref(&self) -> ReflectRef<'_> {
        ReflectRef::Array(self)
    }

    fn reflect_mut(&mut self) -> ReflectMut<'_> {
        ReflectMut::Array(self)
    }
}

impl<T: Typed, const N: usize> Array for [T; N] {
    fn len(&self) -> usize {
        N
    }

    fn get(&self, index: usize) -> Option<&dyn Reflect> {
        self.as_slice().get(index).map(|item| item as &dyn Reflect)
    }

    fn get_mut(&mut self, index: usize) -> Option<&mut dyn Reflect> {
        self.as_mut_slice()
            .get_mut(index)
            .map(|item| item as &mut dyn Reflect)
    }

    fn iter(&self) -> ReflectIter<'_, &dyn Reflect> {
        Box::new(self.as_slice().iter().map(|item| item as &dyn Reflect))
    }
}
