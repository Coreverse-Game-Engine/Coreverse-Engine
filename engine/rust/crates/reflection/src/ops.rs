//! The structural operations behind `Reflect::apply`, `Reflect::clone_value` and `Reflect::reflect_eq`, written
//! against the kind traits so that they work on concrete and on dynamic values alike.

use crate::dynamic::{
    DynamicArray, DynamicEnum, DynamicList, DynamicMap, DynamicSet, DynamicStruct, DynamicTuple,
    DynamicTupleStruct, DynamicVariant,
};
use crate::error::ReflectError;
use crate::info::VariantKind;
use crate::reflect::{Array, Enum, List, Map, Reflect, ReflectMut, ReflectRef, Set, Struct};

/// The items of a list or an array, for building a `Vec` or an array from a reflected value.
pub(crate) fn sequence_items(value: &dyn Reflect) -> Option<Vec<&dyn Reflect>> {
    match value.reflect_ref() {
        ReflectRef::List(list) => Some(list.iter().collect()),
        ReflectRef::Array(array) => Some(array.iter().collect()),
        _ => None,
    }
}

/// See [`Reflect::apply`].
pub(crate) fn apply(target: &mut dyn Reflect, source: &dyn Reflect) -> Result<(), ReflectError> {
    let switches_variant = match (target.reflect_ref(), source.reflect_ref()) {
        (ReflectRef::Enum(to), ReflectRef::Enum(from)) => to.variant_name() != from.variant_name(),
        _ => false,
    };
    if switches_variant {
        return target.try_replace(source);
    }
    let target_kind = target.kind();
    let source_kind = source.kind();
    match (target.reflect_mut(), source.reflect_ref()) {
        (ReflectMut::Struct(to), ReflectRef::Struct(from)) => apply_struct(to, from),
        (ReflectMut::TupleStruct(to), ReflectRef::TupleStruct(from)) => {
            for index in 0..from.field_len() {
                if let (Some(a), Some(b)) = (to.field_mut(index), from.field(index)) {
                    apply(a, b)?;
                }
            }
            Ok(())
        }
        (ReflectMut::Tuple(to), ReflectRef::Tuple(from)) => {
            for index in 0..from.field_len() {
                if let (Some(a), Some(b)) = (to.field_mut(index), from.field(index)) {
                    apply(a, b)?;
                }
            }
            Ok(())
        }
        (ReflectMut::Enum(to), ReflectRef::Enum(from)) => apply_enum_fields(to, from),
        (ReflectMut::List(to), ReflectRef::List(from)) => apply_list(to, from),
        (ReflectMut::Array(to), ReflectRef::Array(from)) => apply_array(to, from),
        (ReflectMut::Map(to), ReflectRef::Map(from)) => apply_map(to, from),
        (ReflectMut::Set(to), ReflectRef::Set(from)) => apply_set(to, from),
        (ReflectMut::Opaque(to), ReflectRef::Opaque(from)) => {
            let copy = from
                .opaque_clone()
                .ok_or_else(|| ReflectError::unsupported("clone", from.type_info().path()))?;
            to.set(copy)
        }
        _ => Err(ReflectError::KindMismatch {
            expected: target_kind,
            actual: source_kind,
        }),
    }
}

fn apply_struct(target: &mut dyn Struct, source: &dyn Struct) -> Result<(), ReflectError> {
    for index in 0..source.field_len() {
        let Some(name) = source.name_at(index) else {
            continue;
        };
        if let (Some(to), Some(from)) = (target.field_mut(name), source.field_at(index)) {
            apply(to, from)?;
        }
    }
    Ok(())
}

fn apply_enum_fields(target: &mut dyn Enum, source: &dyn Enum) -> Result<(), ReflectError> {
    for index in 0..source.field_len() {
        let pair = match source.name_at(index) {
            Some(name) => (target.field_mut(name), source.field_at(index)),
            None => (target.field_at_mut(index), source.field_at(index)),
        };
        if let (Some(to), Some(from)) = pair {
            apply(to, from)?;
        }
    }
    Ok(())
}

fn apply_list(target: &mut dyn List, source: &dyn List) -> Result<(), ReflectError> {
    let wanted = source.len();
    while target.len() > wanted {
        target.pop();
    }
    for index in 0..wanted {
        let Some(from) = source.get(index) else {
            break;
        };
        if let Some(to) = target.get_mut(index) {
            apply(to, from)?;
        } else {
            target.push(clone_value(from)?)?;
        }
    }
    Ok(())
}

fn apply_array(target: &mut dyn Array, source: &dyn Array) -> Result<(), ReflectError> {
    if target.len() != source.len() {
        return Err(ReflectError::LengthMismatch {
            expected: target.len(),
            actual: source.len(),
        });
    }
    for index in 0..source.len() {
        if let (Some(to), Some(from)) = (target.get_mut(index), source.get(index)) {
            apply(to, from)?;
        }
    }
    Ok(())
}

fn apply_map(target: &mut dyn Map, source: &dyn Map) -> Result<(), ReflectError> {
    for (key, value) in source.iter() {
        if let Some(to) = target.get_mut(key) {
            apply(to, value)?;
        } else {
            target.insert(clone_value(key)?, clone_value(value)?)?;
        }
    }
    Ok(())
}

fn apply_set(target: &mut dyn Set, source: &dyn Set) -> Result<(), ReflectError> {
    for item in source.iter() {
        if !target.contains(item) {
            target.insert(clone_value(item)?)?;
        }
    }
    Ok(())
}

/// See [`Reflect::clone_value`].
pub(crate) fn clone_value(value: &dyn Reflect) -> Result<Box<dyn Reflect>, ReflectError> {
    let represented = value.represented_type_info();
    match value.reflect_ref() {
        ReflectRef::Struct(view) => {
            let mut out = DynamicStruct::new();
            out.set_represented_type(represented);
            for index in 0..view.field_len() {
                if let (Some(name), Some(field)) = (view.name_at(index), view.field_at(index)) {
                    out.insert_boxed(name, clone_value(field)?);
                }
            }
            Ok(Box::new(out) as Box<dyn Reflect>)
        }
        ReflectRef::TupleStruct(view) => {
            let mut out = DynamicTupleStruct::new();
            out.set_represented_type(represented);
            for index in 0..view.field_len() {
                if let Some(field) = view.field(index) {
                    out.push_boxed(clone_value(field)?);
                }
            }
            Ok(Box::new(out) as Box<dyn Reflect>)
        }
        ReflectRef::Tuple(view) => {
            let mut out = DynamicTuple::new();
            out.set_represented_type(represented);
            for index in 0..view.field_len() {
                if let Some(field) = view.field(index) {
                    out.push_boxed(clone_value(field)?);
                }
            }
            Ok(Box::new(out) as Box<dyn Reflect>)
        }
        ReflectRef::Enum(view) => {
            let data = match view.variant_kind() {
                VariantKind::Unit => DynamicVariant::Unit,
                VariantKind::Tuple => {
                    let mut items = Vec::new();
                    for index in 0..view.field_len() {
                        if let Some(field) = view.field_at(index) {
                            items.push(clone_value(field)?);
                        }
                    }
                    DynamicVariant::Tuple(items)
                }
                VariantKind::Struct => {
                    let mut fields = Vec::new();
                    for index in 0..view.field_len() {
                        if let (Some(name), Some(field)) =
                            (view.name_at(index), view.field_at(index))
                        {
                            fields.push((name, clone_value(field)?));
                        }
                    }
                    DynamicVariant::Struct(fields)
                }
            };
            let mut out = DynamicEnum::new(view.variant_name(), data);
            out.set_index(view.variant_index());
            out.set_represented_type(represented);
            Ok(Box::new(out) as Box<dyn Reflect>)
        }
        ReflectRef::List(view) => {
            let mut items = Vec::new();
            for item in view.iter() {
                items.push(clone_value(item)?);
            }
            let mut out = DynamicList::from_items(items);
            out.set_represented_type(represented);
            Ok(Box::new(out) as Box<dyn Reflect>)
        }
        ReflectRef::Array(view) => {
            let mut items = Vec::new();
            for item in view.iter() {
                items.push(clone_value(item)?);
            }
            let mut out = DynamicArray::from_items(items);
            out.set_represented_type(represented);
            Ok(Box::new(out) as Box<dyn Reflect>)
        }
        ReflectRef::Map(view) => {
            let mut entries = Vec::new();
            for (key, item) in view.iter() {
                entries.push((clone_value(key)?, clone_value(item)?));
            }
            let mut out = DynamicMap::from_entries(entries);
            out.set_represented_type(represented);
            Ok(Box::new(out) as Box<dyn Reflect>)
        }
        ReflectRef::Set(view) => {
            let mut items = Vec::new();
            for item in view.iter() {
                items.push(clone_value(item)?);
            }
            let mut out = DynamicSet::from_items(items);
            out.set_represented_type(represented);
            Ok(Box::new(out) as Box<dyn Reflect>)
        }
        ReflectRef::Opaque(view) => view
            .opaque_clone()
            .ok_or_else(|| ReflectError::unsupported("clone", view.type_info().path())),
    }
}

/// `Some(false)` if one result is `Some(false)`, `None` if one is `None` and none is `Some(false)`.
fn conjunction(results: impl IntoIterator<Item = Option<bool>>) -> Option<bool> {
    let mut undecided = false;
    for result in results {
        match result {
            Some(false) => return Some(false),
            Some(true) => {}
            None => undecided = true,
        }
    }
    if undecided { None } else { Some(true) }
}

/// See [`Reflect::reflect_eq`].
pub(crate) fn reflect_eq(a: &dyn Reflect, b: &dyn Reflect) -> Option<bool> {
    match (a.reflect_ref(), b.reflect_ref()) {
        (ReflectRef::Struct(x), ReflectRef::Struct(y)) => {
            if x.field_len() != y.field_len() {
                return Some(false);
            }
            conjunction((0..x.field_len()).map(|index| {
                let (Some(name), Some(left)) = (x.name_at(index), x.field_at(index)) else {
                    return Some(false);
                };
                match y.field(name) {
                    Some(right) => reflect_eq(left, right),
                    None => Some(false),
                }
            }))
        }
        (ReflectRef::TupleStruct(x), ReflectRef::TupleStruct(y)) => {
            if x.field_len() != y.field_len() {
                return Some(false);
            }
            conjunction(
                (0..x.field_len()).map(|index| match (x.field(index), y.field(index)) {
                    (Some(left), Some(right)) => reflect_eq(left, right),
                    _ => Some(false),
                }),
            )
        }
        (ReflectRef::Tuple(x), ReflectRef::Tuple(y)) => {
            if x.field_len() != y.field_len() {
                return Some(false);
            }
            conjunction(
                (0..x.field_len()).map(|index| match (x.field(index), y.field(index)) {
                    (Some(left), Some(right)) => reflect_eq(left, right),
                    _ => Some(false),
                }),
            )
        }
        (ReflectRef::Enum(x), ReflectRef::Enum(y)) => {
            if x.variant_name() != y.variant_name() || x.field_len() != y.field_len() {
                return Some(false);
            }
            conjunction((0..x.field_len()).map(|index| {
                let Some(left) = x.field_at(index) else {
                    return Some(false);
                };
                let right = match x.name_at(index) {
                    Some(name) => y.field(name),
                    None => y.field_at(index),
                };
                match right {
                    Some(right) => reflect_eq(left, right),
                    None => Some(false),
                }
            }))
        }
        (ReflectRef::List(x), ReflectRef::List(y)) => {
            if x.len() != y.len() {
                return Some(false);
            }
            conjunction(
                x.iter()
                    .zip(y.iter())
                    .map(|(left, right)| reflect_eq(left, right)),
            )
        }
        (ReflectRef::Array(x), ReflectRef::Array(y)) => {
            if x.len() != y.len() {
                return Some(false);
            }
            conjunction(
                x.iter()
                    .zip(y.iter())
                    .map(|(left, right)| reflect_eq(left, right)),
            )
        }
        (ReflectRef::Map(x), ReflectRef::Map(y)) => {
            if x.len() != y.len() {
                return Some(false);
            }
            conjunction(x.iter().map(|(key, left)| match y.get(key) {
                Some(right) => reflect_eq(left, right),
                None => Some(false),
            }))
        }
        (ReflectRef::Set(x), ReflectRef::Set(y)) => {
            if x.len() != y.len() {
                return Some(false);
            }
            conjunction(x.iter().map(|item| Some(y.contains(item))))
        }
        (ReflectRef::Opaque(x), ReflectRef::Opaque(_)) => x.opaque_eq(b),
        _ => Some(false),
    }
}
