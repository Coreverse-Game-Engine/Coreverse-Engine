//! Dynamic values: reflected values that are built at run time instead of being a Rust struct, list or map.
//!
//! `Reflect::clone_value` makes them, deserializers will make them, and a *patch* (a `DynamicStruct` with only
//! the fields to change) can be given to `Reflect::apply`. A dynamic value remembers the type it stands for
//! ([`Reflect::represented_type_info`]) when it knows it; `Typed::from_reflect` turns it into a value of that
//! type.
//!
//! The `TypeInfo` of a dynamic type is an opaque description (`"DynamicStruct"`, ...): what the value looks
//! like is only known at run time. `Reflect::kind` and `reflect_ref` tell what it behaves like.
//!
//! Field and variant names are interned (leaked once per distinct name) because the kind traits hand out
//! `&'static str` names.

use std::fmt;

use crate::cell::TypeInfoCell;
use crate::error::ReflectError;
use crate::info::{OpaqueInfo, TypeInfo, VariantKind};
use crate::key::TypeKey;
use crate::reflect::{
    Array, Enum, List, Map, Reflect, ReflectIter, ReflectKind, ReflectMut, ReflectRef, Set, Struct,
    Tuple, TupleStruct, Typed,
};

/// A `'static` copy of `name`, the same one for equal strings.
fn intern_name(name: &str) -> &'static str {
    TypeKey::from_owned(name).path()
}

macro_rules! impl_dynamic {
    ($ty:ident, $kind:ident, $path:literal) => {
        impl Typed for $ty {
            fn static_type_info() -> &'static TypeInfo {
                static CELL: TypeInfoCell = TypeInfoCell::new();
                CELL.get_or_init(|| TypeInfo::Opaque(OpaqueInfo::new::<$ty>(TypeKey::new($path))))
            }
        }

        impl Reflect for $ty {
            crate::impl_reflect_boilerplate!();

            fn kind(&self) -> ReflectKind {
                ReflectKind::$kind
            }

            fn represented_type_info(&self) -> Option<&'static TypeInfo> {
                self.represented
            }

            fn reflect_ref(&self) -> ReflectRef<'_> {
                ReflectRef::$kind(self)
            }

            fn reflect_mut(&mut self) -> ReflectMut<'_> {
                ReflectMut::$kind(self)
            }
        }
    };
}

/// A struct built at run time: named fields in order.
#[derive(Default)]
pub struct DynamicStruct {
    represented: Option<&'static TypeInfo>,
    fields: Vec<(&'static str, Box<dyn Reflect>)>,
}

impl DynamicStruct {
    /// An empty dynamic struct that stands for no type.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the type this value stands for.
    pub fn set_represented_type(&mut self, info: Option<&'static TypeInfo>) {
        self.represented = info;
    }

    /// Sets the field `name` to `value`; a field of that name is replaced, a new one goes last.
    pub fn insert<V: Reflect>(&mut self, name: &str, value: V) {
        self.insert_boxed(name, Box::new(value));
    }

    /// Like [`DynamicStruct::insert`] for a boxed value.
    pub fn insert_boxed(&mut self, name: &str, value: Box<dyn Reflect>) {
        let name = intern_name(name);
        match self
            .fields
            .iter()
            .position(|(existing, _)| *existing == name)
        {
            Some(index) => self.fields[index].1 = value,
            None => self.fields.push((name, value)),
        }
    }

    /// The number of fields.
    pub fn len(&self) -> usize {
        self.fields.len()
    }

    /// Whether there are no fields.
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    fn index_of(&self, name: &str) -> Option<usize> {
        self.fields
            .iter()
            .position(|(existing, _)| *existing == name)
    }
}

impl Struct for DynamicStruct {
    fn field_len(&self) -> usize {
        self.fields.len()
    }

    fn name_at(&self, index: usize) -> Option<&'static str> {
        self.fields.get(index).map(|(name, _)| *name)
    }

    fn field(&self, name: &str) -> Option<&dyn Reflect> {
        let index = self.index_of(name)?;
        self.field_at(index)
    }

    fn field_mut(&mut self, name: &str) -> Option<&mut dyn Reflect> {
        let index = self.index_of(name)?;
        self.field_at_mut(index)
    }

    fn field_at(&self, index: usize) -> Option<&dyn Reflect> {
        self.fields.get(index).map(|(_, value)| &**value)
    }

    fn field_at_mut(&mut self, index: usize) -> Option<&mut dyn Reflect> {
        self.fields.get_mut(index).map(|(_, value)| &mut **value)
    }
}

impl fmt::Debug for DynamicStruct {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DynamicStruct")
            .field("fields", &self.fields.len())
            .finish()
    }
}

impl_dynamic!(DynamicStruct, Struct, "DynamicStruct");

/// A tuple struct built at run time: unnamed fields in order.
#[derive(Default)]
pub struct DynamicTupleStruct {
    represented: Option<&'static TypeInfo>,
    fields: Vec<Box<dyn Reflect>>,
}

impl DynamicTupleStruct {
    /// An empty dynamic tuple struct that stands for no type.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the type this value stands for.
    pub fn set_represented_type(&mut self, info: Option<&'static TypeInfo>) {
        self.represented = info;
    }

    /// Appends a field.
    pub fn push<V: Reflect>(&mut self, value: V) {
        self.fields.push(Box::new(value));
    }

    /// Appends a boxed field.
    pub fn push_boxed(&mut self, value: Box<dyn Reflect>) {
        self.fields.push(value);
    }

    /// The number of fields.
    pub fn len(&self) -> usize {
        self.fields.len()
    }

    /// Whether there are no fields.
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }
}

impl TupleStruct for DynamicTupleStruct {
    fn field_len(&self) -> usize {
        self.fields.len()
    }

    fn field(&self, index: usize) -> Option<&dyn Reflect> {
        self.fields.get(index).map(|value| &**value)
    }

    fn field_mut(&mut self, index: usize) -> Option<&mut dyn Reflect> {
        self.fields.get_mut(index).map(|value| &mut **value)
    }
}

impl fmt::Debug for DynamicTupleStruct {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DynamicTupleStruct")
            .field("fields", &self.fields.len())
            .finish()
    }
}

impl_dynamic!(DynamicTupleStruct, TupleStruct, "DynamicTupleStruct");

/// A tuple built at run time: unnamed fields in order.
#[derive(Default)]
pub struct DynamicTuple {
    represented: Option<&'static TypeInfo>,
    fields: Vec<Box<dyn Reflect>>,
}

impl DynamicTuple {
    /// An empty dynamic tuple that stands for no type.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the type this value stands for.
    pub fn set_represented_type(&mut self, info: Option<&'static TypeInfo>) {
        self.represented = info;
    }

    /// Appends a field.
    pub fn push<V: Reflect>(&mut self, value: V) {
        self.fields.push(Box::new(value));
    }

    /// Appends a boxed field.
    pub fn push_boxed(&mut self, value: Box<dyn Reflect>) {
        self.fields.push(value);
    }

    /// The number of fields.
    pub fn len(&self) -> usize {
        self.fields.len()
    }

    /// Whether there are no fields.
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }
}

impl Tuple for DynamicTuple {
    fn field_len(&self) -> usize {
        self.fields.len()
    }

    fn field(&self, index: usize) -> Option<&dyn Reflect> {
        self.fields.get(index).map(|value| &**value)
    }

    fn field_mut(&mut self, index: usize) -> Option<&mut dyn Reflect> {
        self.fields.get_mut(index).map(|value| &mut **value)
    }
}

impl fmt::Debug for DynamicTuple {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DynamicTuple")
            .field("fields", &self.fields.len())
            .finish()
    }
}

impl_dynamic!(DynamicTuple, Tuple, "DynamicTuple");

/// The fields of the current variant of a [`DynamicEnum`].
pub enum DynamicVariant {
    /// A variant without fields.
    Unit,
    /// A variant with unnamed fields.
    Tuple(Vec<Box<dyn Reflect>>),
    /// A variant with named fields.
    Struct(Vec<(&'static str, Box<dyn Reflect>)>),
}

/// An enum value built at run time: the name of a variant and its fields.
pub struct DynamicEnum {
    represented: Option<&'static TypeInfo>,
    variant: &'static str,
    index: usize,
    data: DynamicVariant,
}

impl DynamicEnum {
    /// A value of the variant `name` with the given fields. The variant index is 0 until
    /// [`DynamicEnum::set_index`] says otherwise.
    pub fn new(name: &str, data: DynamicVariant) -> Self {
        Self {
            represented: None,
            variant: intern_name(name),
            index: 0,
            data,
        }
    }

    /// A value of the unit variant `name`.
    pub fn unit(name: &str) -> Self {
        Self::new(name, DynamicVariant::Unit)
    }

    /// Sets the position of the variant (declaration order, starting at 0).
    pub fn set_index(&mut self, index: usize) {
        self.index = index;
    }

    /// Sets the type this value stands for.
    pub fn set_represented_type(&mut self, info: Option<&'static TypeInfo>) {
        self.represented = info;
    }
}

impl Enum for DynamicEnum {
    fn variant_name(&self) -> &'static str {
        self.variant
    }

    fn variant_index(&self) -> usize {
        self.index
    }

    fn variant_kind(&self) -> VariantKind {
        match &self.data {
            DynamicVariant::Unit => VariantKind::Unit,
            DynamicVariant::Tuple(_) => VariantKind::Tuple,
            DynamicVariant::Struct(_) => VariantKind::Struct,
        }
    }

    fn field_len(&self) -> usize {
        match &self.data {
            DynamicVariant::Unit => 0,
            DynamicVariant::Tuple(items) => items.len(),
            DynamicVariant::Struct(fields) => fields.len(),
        }
    }

    fn name_at(&self, index: usize) -> Option<&'static str> {
        match &self.data {
            DynamicVariant::Struct(fields) => fields.get(index).map(|(name, _)| *name),
            _ => None,
        }
    }

    fn field(&self, name: &str) -> Option<&dyn Reflect> {
        match &self.data {
            DynamicVariant::Struct(fields) => {
                let index = fields.iter().position(|(existing, _)| *existing == name)?;
                self.field_at(index)
            }
            _ => None,
        }
    }

    fn field_mut(&mut self, name: &str) -> Option<&mut dyn Reflect> {
        let index = match &self.data {
            DynamicVariant::Struct(fields) => {
                fields.iter().position(|(existing, _)| *existing == name)?
            }
            _ => return None,
        };
        self.field_at_mut(index)
    }

    fn field_at(&self, index: usize) -> Option<&dyn Reflect> {
        match &self.data {
            DynamicVariant::Unit => None,
            DynamicVariant::Tuple(items) => items.get(index).map(|value| &**value),
            DynamicVariant::Struct(fields) => fields.get(index).map(|(_, value)| &**value),
        }
    }

    fn field_at_mut(&mut self, index: usize) -> Option<&mut dyn Reflect> {
        match &mut self.data {
            DynamicVariant::Unit => None,
            DynamicVariant::Tuple(items) => items.get_mut(index).map(|value| &mut **value),
            DynamicVariant::Struct(fields) => fields.get_mut(index).map(|(_, value)| &mut **value),
        }
    }
}

impl fmt::Debug for DynamicEnum {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DynamicEnum")
            .field("variant", &self.variant)
            .finish()
    }
}

impl_dynamic!(DynamicEnum, Enum, "DynamicEnum");

/// A growable sequence built at run time.
#[derive(Default)]
pub struct DynamicList {
    represented: Option<&'static TypeInfo>,
    items: Vec<Box<dyn Reflect>>,
}

impl DynamicList {
    /// An empty dynamic list that stands for no type.
    pub fn new() -> Self {
        Self::default()
    }

    /// A dynamic list with the given items.
    pub fn from_items(items: Vec<Box<dyn Reflect>>) -> Self {
        Self {
            represented: None,
            items,
        }
    }

    /// Sets the type this value stands for.
    pub fn set_represented_type(&mut self, info: Option<&'static TypeInfo>) {
        self.represented = info;
    }
}

impl List for DynamicList {
    fn len(&self) -> usize {
        self.items.len()
    }

    fn get(&self, index: usize) -> Option<&dyn Reflect> {
        self.items.get(index).map(|value| &**value)
    }

    fn get_mut(&mut self, index: usize) -> Option<&mut dyn Reflect> {
        self.items.get_mut(index).map(|value| &mut **value)
    }

    fn push(&mut self, value: Box<dyn Reflect>) -> Result<(), ReflectError> {
        self.items.push(value);
        Ok(())
    }

    fn insert(&mut self, index: usize, value: Box<dyn Reflect>) -> Result<(), ReflectError> {
        let len = self.items.len();
        if index > len {
            return Err(ReflectError::index_out_of_bounds(index, len));
        }
        self.items.insert(index, value);
        Ok(())
    }

    fn pop(&mut self) -> Option<Box<dyn Reflect>> {
        self.items.pop()
    }

    fn remove(&mut self, index: usize) -> Option<Box<dyn Reflect>> {
        if index >= self.items.len() {
            return None;
        }
        Some(self.items.remove(index))
    }

    fn clear(&mut self) {
        self.items.clear();
    }

    fn iter(&self) -> ReflectIter<'_, &dyn Reflect> {
        Box::new(self.items.iter().map(|value| &**value))
    }
}

impl fmt::Debug for DynamicList {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DynamicList")
            .field("items", &self.items.len())
            .finish()
    }
}

impl_dynamic!(DynamicList, List, "DynamicList");

/// A fixed-size sequence built at run time.
#[derive(Default)]
pub struct DynamicArray {
    represented: Option<&'static TypeInfo>,
    items: Vec<Box<dyn Reflect>>,
}

impl DynamicArray {
    /// A dynamic array with the given items.
    pub fn from_items(items: Vec<Box<dyn Reflect>>) -> Self {
        Self {
            represented: None,
            items,
        }
    }

    /// Sets the type this value stands for.
    pub fn set_represented_type(&mut self, info: Option<&'static TypeInfo>) {
        self.represented = info;
    }
}

impl Array for DynamicArray {
    fn len(&self) -> usize {
        self.items.len()
    }

    fn get(&self, index: usize) -> Option<&dyn Reflect> {
        self.items.get(index).map(|value| &**value)
    }

    fn get_mut(&mut self, index: usize) -> Option<&mut dyn Reflect> {
        self.items.get_mut(index).map(|value| &mut **value)
    }

    fn iter(&self) -> ReflectIter<'_, &dyn Reflect> {
        Box::new(self.items.iter().map(|value| &**value))
    }
}

impl fmt::Debug for DynamicArray {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DynamicArray")
            .field("items", &self.items.len())
            .finish()
    }
}

impl_dynamic!(DynamicArray, Array, "DynamicArray");

/// A key and a value of a [`DynamicMap`].
type MapEntry = (Box<dyn Reflect>, Box<dyn Reflect>);

/// A map built at run time. Keys are found with `Reflect::reflect_eq`.
#[derive(Default)]
pub struct DynamicMap {
    represented: Option<&'static TypeInfo>,
    entries: Vec<MapEntry>,
}

impl DynamicMap {
    /// An empty dynamic map that stands for no type.
    pub fn new() -> Self {
        Self::default()
    }

    /// A dynamic map with the given entries. The keys must be different; this is not checked.
    pub fn from_entries(entries: Vec<MapEntry>) -> Self {
        Self {
            represented: None,
            entries,
        }
    }

    /// Sets the type this value stands for.
    pub fn set_represented_type(&mut self, info: Option<&'static TypeInfo>) {
        self.represented = info;
    }

    fn index_of(&self, key: &dyn Reflect) -> Option<usize> {
        self.entries
            .iter()
            .position(|(existing, _)| existing.reflect_eq(key) == Some(true))
    }
}

impl Map for DynamicMap {
    fn len(&self) -> usize {
        self.entries.len()
    }

    fn get(&self, key: &dyn Reflect) -> Option<&dyn Reflect> {
        let index = self.index_of(key)?;
        self.entries.get(index).map(|(_, value)| &**value)
    }

    fn get_mut(&mut self, key: &dyn Reflect) -> Option<&mut dyn Reflect> {
        let index = self.index_of(key)?;
        self.entries.get_mut(index).map(|(_, value)| &mut **value)
    }

    fn insert(
        &mut self,
        key: Box<dyn Reflect>,
        value: Box<dyn Reflect>,
    ) -> Result<Option<Box<dyn Reflect>>, ReflectError> {
        match self.index_of(&*key) {
            Some(index) => Ok(Some(std::mem::replace(&mut self.entries[index].1, value))),
            None => {
                self.entries.push((key, value));
                Ok(None)
            }
        }
    }

    fn remove(&mut self, key: &dyn Reflect) -> Option<Box<dyn Reflect>> {
        let index = self.index_of(key)?;
        Some(self.entries.remove(index).1)
    }

    fn clear(&mut self) {
        self.entries.clear();
    }

    fn iter(&self) -> ReflectIter<'_, (&dyn Reflect, &dyn Reflect)> {
        Box::new(self.entries.iter().map(|(key, value)| (&**key, &**value)))
    }
}

impl fmt::Debug for DynamicMap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DynamicMap")
            .field("entries", &self.entries.len())
            .finish()
    }
}

impl_dynamic!(DynamicMap, Map, "DynamicMap");

/// A set built at run time. Items are found with `Reflect::reflect_eq`.
#[derive(Default)]
pub struct DynamicSet {
    represented: Option<&'static TypeInfo>,
    items: Vec<Box<dyn Reflect>>,
}

impl DynamicSet {
    /// An empty dynamic set that stands for no type.
    pub fn new() -> Self {
        Self::default()
    }

    /// A dynamic set with the given items. The items must be different; this is not checked.
    pub fn from_items(items: Vec<Box<dyn Reflect>>) -> Self {
        Self {
            represented: None,
            items,
        }
    }

    /// Sets the type this value stands for.
    pub fn set_represented_type(&mut self, info: Option<&'static TypeInfo>) {
        self.represented = info;
    }

    fn index_of(&self, value: &dyn Reflect) -> Option<usize> {
        self.items
            .iter()
            .position(|existing| existing.reflect_eq(value) == Some(true))
    }
}

impl Set for DynamicSet {
    fn len(&self) -> usize {
        self.items.len()
    }

    fn contains(&self, value: &dyn Reflect) -> bool {
        self.index_of(value).is_some()
    }

    fn insert(&mut self, value: Box<dyn Reflect>) -> Result<bool, ReflectError> {
        if self.index_of(&*value).is_some() {
            return Ok(false);
        }
        self.items.push(value);
        Ok(true)
    }

    fn remove(&mut self, value: &dyn Reflect) -> bool {
        match self.index_of(value) {
            Some(index) => {
                self.items.remove(index);
                true
            }
            None => false,
        }
    }

    fn clear(&mut self) {
        self.items.clear();
    }

    fn iter(&self) -> ReflectIter<'_, &dyn Reflect> {
        Box::new(self.items.iter().map(|value| &**value))
    }
}

impl fmt::Debug for DynamicSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DynamicSet")
            .field("items", &self.items.len())
            .finish()
    }
}

impl_dynamic!(DynamicSet, Set, "DynamicSet");
