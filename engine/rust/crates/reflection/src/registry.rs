//! The type registry: the types a program knows how to reflect, with their type data.

use std::any::{Any, TypeId};
use std::collections::{HashMap, HashSet};
use std::fmt;

use crate::error::ReflectError;
use crate::info::TypeInfo;
use crate::key::TypeKey;
use crate::reflect::{Reflect, Typed};
use crate::type_data::{ReflectClone, ReflectDefault, TypeData};

/// One registered type: its description and its type data.
pub struct TypeRegistration {
    info: &'static TypeInfo,
    data: HashMap<TypeId, Box<dyn Any + Send + Sync>>,
}

impl TypeRegistration {
    /// A registration of the type with the given description and no type data.
    pub fn new(info: &'static TypeInfo) -> Self {
        Self {
            info,
            data: HashMap::new(),
        }
    }

    /// A registration of `T`: its description and the type data `T` asks for
    /// ([`Typed::register_type_data`]).
    pub fn of<T: Typed>() -> Self {
        let mut registration = Self::new(T::static_type_info());
        T::register_type_data(&mut registration);
        registration
    }

    /// The description of the type.
    pub fn info(&self) -> &'static TypeInfo {
        self.info
    }

    /// The stable key of the type.
    pub fn key(&self) -> TypeKey {
        self.info.key()
    }

    /// The path of the type.
    pub fn path(&self) -> &'static str {
        self.info.path()
    }

    /// The standard `TypeId` of the type.
    pub fn std_type_id(&self) -> TypeId {
        self.info.std_type_id()
    }

    /// Stores type data; type data of the same kind that was there is replaced.
    pub fn insert_data<D: TypeData>(&mut self, data: D) {
        self.data.insert(TypeId::of::<D>(), Box::new(data));
    }

    /// The type data of kind `D`.
    pub fn data<D: TypeData>(&self) -> Option<&D> {
        self.data
            .get(&TypeId::of::<D>())
            .and_then(|data| data.downcast_ref::<D>())
    }

    /// The type data of kind `D`, for changing.
    pub fn data_mut<D: TypeData>(&mut self) -> Option<&mut D> {
        self.data
            .get_mut(&TypeId::of::<D>())
            .and_then(|data| data.downcast_mut::<D>())
    }

    /// Whether the type has type data of kind `D`.
    pub fn has_data<D: TypeData>(&self) -> bool {
        self.data.contains_key(&TypeId::of::<D>())
    }

    /// Removes the type data of kind `D`; `true` if there was any.
    pub fn remove_data<D: TypeData>(&mut self) -> bool {
        self.data.remove(&TypeId::of::<D>()).is_some()
    }

    /// The number of kinds of type data the type has.
    pub fn data_len(&self) -> usize {
        self.data.len()
    }
}

impl fmt::Debug for TypeRegistration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TypeRegistration")
            .field("path", &self.path())
            .field("data", &self.data.len())
            .finish()
    }
}

/// What the derive submits to `inventory` for every non-generic type; `TypeRegistry::from_inventory` collects
/// them. Not meant to be used by hand.
#[doc(hidden)]
pub struct TypeEntry {
    /// Builds the registration of the type.
    pub registration: fn() -> TypeRegistration,
}

impl TypeEntry {
    /// An entry for the given registration function.
    pub const fn new(registration: fn() -> TypeRegistration) -> Self {
        Self { registration }
    }
}

inventory::collect!(TypeEntry);

/// The paths and hashes in use, to find two types that claim the same name.
#[derive(Default)]
struct NameIndex {
    /// Hash of a path -> (type, path of the type).
    ids: HashMap<u64, (TypeId, &'static str)>,
    /// A path or an alias -> (type, path of the type).
    names: HashMap<&'static str, (TypeId, &'static str)>,
}

impl NameIndex {
    fn check(&self, registration: &TypeRegistration) -> Result<(), ReflectError> {
        let std_id = registration.std_type_id();
        let path = registration.path();
        let aliases = registration.info().aliases().iter().copied();
        for name in std::iter::once(path).chain(aliases) {
            let taken = self.names.get(name).filter(|entry| entry.0 != std_id);
            if let Some(&(_, owner_path)) = taken {
                return Err(ReflectError::NameConflict {
                    name,
                    existing: owner_path,
                    new: path,
                });
            }
        }
        let id = registration.key().id();
        let taken = self.ids.get(&id).filter(|entry| entry.0 != std_id);
        if let Some(&(_, owner_path)) = taken {
            return Err(ReflectError::HashCollision {
                id,
                existing: owner_path,
                new: path,
            });
        }
        Ok(())
    }

    fn add(&mut self, registration: &TypeRegistration) {
        let owner = (registration.std_type_id(), registration.path());
        self.ids.insert(registration.key().id(), owner);
        self.names.insert(registration.path(), owner);
        for alias in registration.info().aliases().iter().copied() {
            self.names.insert(alias, owner);
        }
    }
}

/// The types a program knows how to reflect, found by their standard `TypeId`, their path or alias, or the hash
/// of their path, together with their type data.
///
/// There is no global registry: build one with [`TypeRegistry::new`] and [`register`](TypeRegistry::register),
/// or collect every derived non-generic type with [`TypeRegistry::from_inventory`]. Registering a type also
/// registers the types it is made of. Two different types may not share a path or an alias.
#[derive(Default)]
pub struct TypeRegistry {
    registrations: HashMap<TypeId, TypeRegistration>,
    index: NameIndex,
}

impl TypeRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// A registry with every type that derives `Reflect` (non-generic ones; generic instantiations are
    /// registered when something registered contains them, or by hand) and the types they are made of.
    ///
    /// # Errors
    ///
    /// The first [`ReflectError::NameConflict`] or [`ReflectError::HashCollision`]. The types are registered in
    /// the order of their paths, so the same program gives the same error every time.
    pub fn from_inventory() -> Result<Self, ReflectError> {
        let mut roots: Vec<TypeRegistration> = Vec::new();
        for entry in inventory::iter::<TypeEntry> {
            roots.push((entry.registration)());
        }
        roots.sort_by(|a, b| a.path().cmp(b.path()));
        let mut registry = Self::new();
        registry.add_all(roots)?;
        Ok(registry)
    }

    /// Registers `T` and the types it is made of (with the type data each asks for). A type that is already
    /// registered is left as it is.
    ///
    /// # Errors
    ///
    /// [`ReflectError::NameConflict`] or [`ReflectError::HashCollision`] if one of the types clashes with a
    /// registered one. Nothing is registered then.
    pub fn register<T: Typed>(&mut self) -> Result<(), ReflectError> {
        self.add_all(vec![TypeRegistration::of::<T>()])
    }

    /// Adds (or replaces) type data of `T`, registering `T` first if needed.
    ///
    /// # Errors
    ///
    /// The errors of [`register`](TypeRegistry::register).
    pub fn add_type_data<T: Typed, D: TypeData>(&mut self, data: D) -> Result<(), ReflectError> {
        self.register::<T>()?;
        if let Some(registration) = self.registrations.get_mut(&TypeId::of::<T>()) {
            registration.insert_data(data);
        }
        Ok(())
    }

    /// Registers the roots and, before them, everything they are made of; all of it or nothing.
    fn add_all(&mut self, roots: Vec<TypeRegistration>) -> Result<(), ReflectError> {
        let mut pending: Vec<TypeRegistration> = Vec::new();
        let mut seen: HashSet<TypeId> = HashSet::new();
        let mut stack = roots;
        stack.reverse();
        while let Some(registration) = stack.pop() {
            let id = registration.std_type_id();
            if self.registrations.contains_key(&id) || !seen.insert(id) {
                continue;
            }
            for dependency in registration.info().dependencies() {
                let dependency_id = dependency.std_type_id();
                if !self.registrations.contains_key(&dependency_id)
                    && !seen.contains(&dependency_id)
                {
                    stack.push(dependency.registration());
                }
            }
            pending.push(registration);
        }
        let mut staged = NameIndex::default();
        for registration in &pending {
            self.index.check(registration)?;
            staged.check(registration)?;
            staged.add(registration);
        }
        for registration in pending {
            self.index.add(&registration);
            self.registrations
                .insert(registration.std_type_id(), registration);
        }
        Ok(())
    }

    /// The number of registered types.
    pub fn len(&self) -> usize {
        self.registrations.len()
    }

    /// Whether no type is registered.
    pub fn is_empty(&self) -> bool {
        self.registrations.is_empty()
    }

    /// The registered types, in no particular order.
    pub fn iter(&self) -> impl Iterator<Item = &TypeRegistration> {
        self.registrations.values()
    }

    /// The registration of the type with the given standard `TypeId`.
    pub fn get(&self, type_id: TypeId) -> Option<&TypeRegistration> {
        self.registrations.get(&type_id)
    }

    /// The registration of `T`.
    pub fn get_by_type<T: Any>(&self) -> Option<&TypeRegistration> {
        self.get(TypeId::of::<T>())
    }

    /// Whether `T` is registered.
    pub fn contains<T: Any>(&self) -> bool {
        self.registrations.contains_key(&TypeId::of::<T>())
    }

    /// The registration of the type with the given path or alias.
    pub fn get_by_path(&self, name: &str) -> Option<&TypeRegistration> {
        let (type_id, _) = self.index.names.get(name)?;
        self.registrations.get(type_id)
    }

    /// The registration of the type whose path has the given 64-bit hash.
    pub fn get_by_id(&self, id: u64) -> Option<&TypeRegistration> {
        let (type_id, _) = self.index.ids.get(&id)?;
        self.registrations.get(type_id)
    }

    /// The registration of the type with the given key.
    pub fn get_by_key(&self, key: &TypeKey) -> Option<&TypeRegistration> {
        self.get_by_id(key.id())
    }

    /// The type data of kind `D` of `T`.
    pub fn data<T: Any, D: TypeData>(&self) -> Option<&D> {
        self.get_by_type::<T>()?.data::<D>()
    }

    /// A new default value of the type with the given path or alias.
    ///
    /// # Errors
    ///
    /// [`ReflectError::UnknownType`] if no such type is registered, [`ReflectError::MissingTypeData`] if it has
    /// no [`ReflectDefault`].
    pub fn create_default(&self, name: &str) -> Result<Box<dyn Reflect>, ReflectError> {
        let registration = self
            .get_by_path(name)
            .ok_or_else(|| ReflectError::UnknownType {
                name: name.to_string(),
            })?;
        let default =
            registration
                .data::<ReflectDefault>()
                .ok_or(ReflectError::MissingTypeData {
                    path: registration.path(),
                    data: "ReflectDefault",
                })?;
        Ok(default.create())
    }

    /// A clone of `value`, made with the [`ReflectClone`] of its type.
    ///
    /// # Errors
    ///
    /// [`ReflectError::UnknownType`] if the type of `value` is not registered,
    /// [`ReflectError::MissingTypeData`] if it has no [`ReflectClone`].
    pub fn clone_value(&self, value: &dyn Reflect) -> Result<Box<dyn Reflect>, ReflectError> {
        let info = value.type_info();
        let registration =
            self.get(info.std_type_id())
                .ok_or_else(|| ReflectError::UnknownType {
                    name: info.path().to_string(),
                })?;
        let clone = registration
            .data::<ReflectClone>()
            .ok_or(ReflectError::MissingTypeData {
                path: registration.path(),
                data: "ReflectClone",
            })?;
        clone.clone_value(value)
    }
}

impl fmt::Debug for TypeRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TypeRegistry")
            .field("types", &self.registrations.len())
            .finish()
    }
}
