//! Storage for the `'static` type descriptions.

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

use utilities::sync::{read, write};

use crate::info::TypeInfo;

/// Holds the [`TypeInfo`] of one non-generic type, built on first use.
///
/// Declared as a `static` inside [`Typed::static_type_info`](crate::Typed::static_type_info):
/// `static CELL: TypeInfoCell = TypeInfoCell::new();`. The function that builds the description must not ask
/// for the description of the same type (fields refer to other types through [`TypeRef`](crate::TypeRef),
/// which is lazy, so this does not come up).
#[derive(Debug, Default)]
pub struct TypeInfoCell {
    cell: OnceLock<TypeInfo>,
}

impl TypeInfoCell {
    /// An empty cell.
    pub const fn new() -> Self {
        Self {
            cell: OnceLock::new(),
        }
    }

    /// The stored description; `init` builds it the first time.
    pub fn get_or_init(&'static self, init: impl FnOnce() -> TypeInfo) -> &'static TypeInfo {
        self.cell.get_or_init(init)
    }
}

/// Holds the [`TypeInfo`] of every instantiation of one generic type (`Vec<u32>`, `Vec<String>`, ...).
///
/// A `static` inside a generic function is one static for all instantiations, so the descriptions are kept
/// in a map keyed by the standard `TypeId` of the instantiation. A description is leaked on purpose: there is
/// at most one per instantiation that is used, and it has to live as long as the program.
#[derive(Debug, Default)]
pub struct GenericTypeInfoCell {
    map: OnceLock<RwLock<HashMap<TypeId, &'static TypeInfo>>>,
}

impl GenericTypeInfoCell {
    /// An empty cell.
    pub const fn new() -> Self {
        Self {
            map: OnceLock::new(),
        }
    }

    /// The description of the instantiation `K`; `init` builds it the first time.
    ///
    /// No lock is held while `init` runs (it asks for the descriptions of the type arguments, which live in
    /// this same cell). If two threads build the same description at once, one result is dropped.
    pub fn get_or_insert<K: Any>(&self, init: impl FnOnce() -> TypeInfo) -> &'static TypeInfo {
        let map = self.map.get_or_init(|| RwLock::new(HashMap::new()));
        let id = TypeId::of::<K>();
        let hit = read(map).get(&id).copied();
        if let Some(info) = hit {
            return info;
        }
        let built = init();
        let mut guard = write(map);
        if let Some(existing) = guard.get(&id).copied() {
            return existing;
        }
        let leaked: &'static TypeInfo = Box::leak(Box::new(built));
        guard.insert(id, leaked);
        leaked
    }
}
