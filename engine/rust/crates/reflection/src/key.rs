//! The stable identity of a reflected type.

use std::collections::HashSet;
use std::fmt;
use std::sync::{Mutex, OnceLock};

use utilities::fnv1a_64;
use utilities::sync::lock;

/// The stable identity of a reflected type: a path such as `"u32"` or `"Vec<String>"` and the 64-bit FNV-1a
/// hash of that path.
///
/// `std::any::TypeId` and `std::any::type_name` can change from one build to the next, so neither may end
/// up in a file or on the network. The key is derived from a name the type's author controls. The built-in
/// implementations use short, fixed paths (`"u32"`, `"String"`, `"Vec<T>"` with `T` replaced by the path of
/// the item type, and so on); they do not depend on the standard library's module layout.
///
/// Two different types with the same hash are detected when the registry is added (milestone R3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TypeKey {
    id: u64,
    path: &'static str,
}

impl TypeKey {
    /// The key of the type with the given path. Usable in `const` and `static` items.
    pub const fn new(path: &'static str) -> Self {
        Self {
            id: fnv1a_64(path.as_bytes()),
            path,
        }
    }

    /// The key of a type whose path is built at run time (generic types: the paths of the type arguments are
    /// part of the path).
    ///
    /// The string is interned: asking twice for the same path leaks one string, not two.
    pub fn from_owned(path: &str) -> Self {
        Self::new(intern(path))
    }

    /// The 64-bit FNV-1a hash of the path.
    pub const fn id(&self) -> u64 {
        self.id
    }

    /// The path the key was made from.
    pub const fn path(&self) -> &'static str {
        self.path
    }
}

impl fmt::Display for TypeKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.path)
    }
}

/// Returns a `'static` copy of `path`, the same one for equal strings.
fn intern(path: &str) -> &'static str {
    static POOL: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    let pool = POOL.get_or_init(|| Mutex::new(HashSet::new()));
    let mut pool = lock(pool);
    if let Some(existing) = pool.get(path).copied() {
        return existing;
    }
    let leaked: &'static str = Box::leak(path.to_owned().into_boxed_str());
    pool.insert(leaked);
    leaked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_path_gives_same_key() {
        assert_eq!(TypeKey::new("u32"), TypeKey::new("u32"));
        assert_ne!(TypeKey::new("u32"), TypeKey::new("u64"));
        assert_eq!(TypeKey::new("u32").id(), fnv1a_64(b"u32"));
    }

    #[test]
    fn owned_paths_are_interned() {
        let a = TypeKey::from_owned(&format!("Vec<{}>", "u32"));
        let b = TypeKey::from_owned("Vec<u32>");
        assert_eq!(a, b);
        assert!(std::ptr::eq(a.path(), b.path()));
        assert_eq!(a.to_string(), "Vec<u32>");
    }
}
