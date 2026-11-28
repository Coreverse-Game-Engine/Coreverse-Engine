//! The static descriptions of reflected types.
//!
//! Every reflected type has one [`TypeInfo`] with `'static` lifetime. It says what the type is (its
//! [`TypeKey`] and [`ReflectKind`]) and what it is made of (fields, variants, item types). Descriptions
//! refer to other types through [`TypeRef`], which only builds the other description when asked; that keeps
//! recursive types (a node that holds an `Option<Box<Node>>`) from looping.

use std::any::{Any, TypeId};
use std::fmt;

use crate::key::TypeKey;
use crate::metadata::Metadata;
use crate::reflect::{ReflectKind, Typed};
use crate::registry::TypeRegistration;

/// A lazy reference to the description of another type.
#[derive(Clone, Copy)]
pub struct TypeRef {
    std_id: TypeId,
    info: fn() -> &'static TypeInfo,
    registration: fn() -> TypeRegistration,
}

impl TypeRef {
    /// A reference to the description of `T`. Nothing is built until [`TypeRef::info`] is called.
    pub fn of<T: Typed>() -> Self {
        Self {
            std_id: TypeId::of::<T>(),
            info: T::static_type_info,
            registration: TypeRegistration::of::<T>,
        }
    }

    /// The description of the referenced type (built on first use).
    pub fn info(&self) -> &'static TypeInfo {
        (self.info)()
    }

    /// A new registration of the referenced type: its description and its type data. The registry uses it to
    /// register the types a registered type is made of.
    pub fn registration(&self) -> TypeRegistration {
        (self.registration)()
    }

    /// The standard `TypeId` of the referenced type; it does not build the description.
    pub fn std_type_id(&self) -> TypeId {
        self.std_id
    }

    /// Whether the reference points at `T`; it does not build the description.
    pub fn is<T: Any>(&self) -> bool {
        self.std_id == TypeId::of::<T>()
    }
}

impl PartialEq for TypeRef {
    fn eq(&self, other: &Self) -> bool {
        self.std_id == other.std_id
    }
}

impl Eq for TypeRef {}

impl fmt::Debug for TypeRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("TypeRef").field(&self.info().path()).finish()
    }
}

/// The parts every description has: the key and the standard `TypeId`.
#[derive(Clone, Debug)]
struct Header {
    key: TypeKey,
    std_id: TypeId,
    aliases: &'static [&'static str],
    metadata: Metadata,
}

impl Header {
    fn new<T: Any>(key: TypeKey) -> Self {
        Self {
            key,
            std_id: TypeId::of::<T>(),
            aliases: &[],
            metadata: Metadata::new(),
        }
    }
}

macro_rules! header_methods {
    () => {
        /// The stable key of the type.
        pub fn key(&self) -> TypeKey {
            self.header.key
        }

        /// The path of the type (the text the key was made from).
        pub fn path(&self) -> &'static str {
            self.header.key.path()
        }

        /// The standard `TypeId` of the type.
        pub fn std_type_id(&self) -> TypeId {
            self.header.std_id
        }

        /// Whether the description is the one of `T`.
        pub fn is<T: Any>(&self) -> bool {
            self.header.std_id == TypeId::of::<T>()
        }

        /// The other paths the type was known by (old names, kept so that old data can still be read);
        /// empty for most types.
        pub fn aliases(&self) -> &'static [&'static str] {
            self.header.aliases
        }

        /// The description with the given aliases.
        #[must_use]
        pub fn with_aliases(mut self, aliases: &'static [&'static str]) -> Self {
            self.header.aliases = aliases;
            self
        }

        /// The metadata of the type (tooltip, category, custom attributes, ...); empty for most types.
        pub fn metadata(&self) -> &Metadata {
            &self.header.metadata
        }

        /// The description with the given metadata.
        #[must_use]
        pub fn with_metadata(mut self, metadata: Metadata) -> Self {
            self.header.metadata = metadata;
            self
        }
    };
}

/// One field of a struct, tuple, tuple struct or enum variant.
#[derive(Clone, Debug)]
pub struct FieldInfo {
    name: Option<&'static str>,
    index: usize,
    ty: TypeRef,
    aliases: &'static [&'static str],
    metadata: Metadata,
}

impl FieldInfo {
    /// A named field at the given position (declaration order, starting at 0).
    pub const fn named(name: &'static str, index: usize, ty: TypeRef) -> Self {
        Self {
            name: Some(name),
            index,
            ty,
            aliases: &[],
            metadata: Metadata::new(),
        }
    }

    /// An unnamed field at the given position.
    pub const fn unnamed(index: usize, ty: TypeRef) -> Self {
        Self {
            name: None,
            index,
            ty,
            aliases: &[],
            metadata: Metadata::new(),
        }
    }

    /// The field with the given aliases: other names it was known by (old names, kept so that old data can
    /// still be read).
    #[must_use]
    pub fn with_aliases(mut self, aliases: &'static [&'static str]) -> Self {
        self.aliases = aliases;
        self
    }

    /// The field with the given metadata.
    #[must_use]
    pub fn with_metadata(mut self, metadata: Metadata) -> Self {
        self.metadata = metadata;
        self
    }

    /// The metadata of the field (tooltip, range, read-only, custom attributes, ...).
    pub fn metadata(&self) -> &Metadata {
        &self.metadata
    }

    /// The name of the field; `None` for the fields of tuples and tuple structs.
    pub const fn name(&self) -> Option<&'static str> {
        self.name
    }

    /// The position of the field (declaration order, starting at 0).
    pub const fn index(&self) -> usize {
        self.index
    }

    /// The type of the field.
    pub const fn ty(&self) -> TypeRef {
        self.ty
    }

    /// The other names the field was known by; empty for most fields.
    pub const fn aliases(&self) -> &'static [&'static str] {
        self.aliases
    }
}

/// Description of a struct with named fields.
#[derive(Clone, Debug)]
pub struct StructInfo {
    header: Header,
    fields: Vec<FieldInfo>,
}

impl StructInfo {
    /// A description of the struct `T`; the fields must be in declaration order and carry names.
    pub fn new<T: Any>(key: TypeKey, fields: Vec<FieldInfo>) -> Self {
        Self {
            header: Header::new::<T>(key),
            fields,
        }
    }

    header_methods!();

    /// The fields, in declaration order.
    pub fn fields(&self) -> &[FieldInfo] {
        &self.fields
    }

    /// The number of fields.
    pub fn field_len(&self) -> usize {
        self.fields.len()
    }

    /// The field with the given name.
    pub fn field(&self, name: &str) -> Option<&FieldInfo> {
        self.fields.iter().find(|field| field.name == Some(name))
    }

    /// The field at the given position.
    pub fn field_at(&self, index: usize) -> Option<&FieldInfo> {
        self.fields.get(index)
    }

    /// The position of the field with the given name.
    pub fn index_of(&self, name: &str) -> Option<usize> {
        self.fields
            .iter()
            .position(|field| field.name == Some(name))
    }
}

/// Description of a tuple struct (`struct Meters(f32)`).
#[derive(Clone, Debug)]
pub struct TupleStructInfo {
    header: Header,
    fields: Vec<FieldInfo>,
}

impl TupleStructInfo {
    /// A description of the tuple struct `T`; the fields must be in declaration order.
    pub fn new<T: Any>(key: TypeKey, fields: Vec<FieldInfo>) -> Self {
        Self {
            header: Header::new::<T>(key),
            fields,
        }
    }

    header_methods!();

    /// The fields, in declaration order.
    pub fn fields(&self) -> &[FieldInfo] {
        &self.fields
    }

    /// The number of fields.
    pub fn field_len(&self) -> usize {
        self.fields.len()
    }

    /// The field at the given position.
    pub fn field_at(&self, index: usize) -> Option<&FieldInfo> {
        self.fields.get(index)
    }
}

/// Description of a tuple (`(f32, f32)`).
#[derive(Clone, Debug)]
pub struct TupleInfo {
    header: Header,
    fields: Vec<FieldInfo>,
}

impl TupleInfo {
    /// A description of the tuple `T`; the fields must be in order.
    pub fn new<T: Any>(key: TypeKey, fields: Vec<FieldInfo>) -> Self {
        Self {
            header: Header::new::<T>(key),
            fields,
        }
    }

    header_methods!();

    /// The fields, in order.
    pub fn fields(&self) -> &[FieldInfo] {
        &self.fields
    }

    /// The number of fields.
    pub fn field_len(&self) -> usize {
        self.fields.len()
    }

    /// The field at the given position.
    pub fn field_at(&self, index: usize) -> Option<&FieldInfo> {
        self.fields.get(index)
    }
}

/// What kind of fields an enum variant has.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum VariantKind {
    /// No fields: `None`.
    Unit,
    /// Unnamed fields: `Some(T)`.
    Tuple,
    /// Named fields: `Move { x: f32, y: f32 }`.
    Struct,
}

/// One variant of an enum.
#[derive(Clone, Debug)]
pub struct VariantInfo {
    name: &'static str,
    index: usize,
    kind: VariantKind,
    fields: Vec<FieldInfo>,
    aliases: &'static [&'static str],
    metadata: Metadata,
}

impl VariantInfo {
    /// A variant without fields.
    pub const fn unit(name: &'static str, index: usize) -> Self {
        Self {
            name,
            index,
            kind: VariantKind::Unit,
            fields: Vec::new(),
            aliases: &[],
            metadata: Metadata::new(),
        }
    }

    /// A variant with unnamed fields.
    pub const fn tuple(name: &'static str, index: usize, fields: Vec<FieldInfo>) -> Self {
        Self {
            name,
            index,
            kind: VariantKind::Tuple,
            fields,
            aliases: &[],
            metadata: Metadata::new(),
        }
    }

    /// A variant with named fields.
    pub const fn with_named_fields(
        name: &'static str,
        index: usize,
        fields: Vec<FieldInfo>,
    ) -> Self {
        Self {
            name,
            index,
            kind: VariantKind::Struct,
            fields,
            aliases: &[],
            metadata: Metadata::new(),
        }
    }

    /// The variant with the given aliases: other names it was known by.
    #[must_use]
    pub fn with_aliases(mut self, aliases: &'static [&'static str]) -> Self {
        self.aliases = aliases;
        self
    }

    /// The variant with the given metadata.
    #[must_use]
    pub fn with_metadata(mut self, metadata: Metadata) -> Self {
        self.metadata = metadata;
        self
    }

    /// The metadata of the variant (tooltip, category, custom attributes, ...).
    pub fn metadata(&self) -> &Metadata {
        &self.metadata
    }

    /// The name of the variant.
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// The position of the variant (declaration order, starting at 0).
    pub const fn index(&self) -> usize {
        self.index
    }

    /// The kind of fields the variant has.
    pub const fn kind(&self) -> VariantKind {
        self.kind
    }

    /// The other names the variant was known by; empty for most variants.
    pub const fn aliases(&self) -> &'static [&'static str] {
        self.aliases
    }

    /// The fields of the variant, in declaration order.
    pub fn fields(&self) -> &[FieldInfo] {
        &self.fields
    }

    /// The number of fields.
    pub fn field_len(&self) -> usize {
        self.fields.len()
    }

    /// The field with the given name (variants with named fields only).
    pub fn field(&self, name: &str) -> Option<&FieldInfo> {
        self.fields.iter().find(|field| field.name == Some(name))
    }

    /// The field at the given position.
    pub fn field_at(&self, index: usize) -> Option<&FieldInfo> {
        self.fields.get(index)
    }
}

/// Description of an enum.
#[derive(Clone, Debug)]
pub struct EnumInfo {
    header: Header,
    variants: Vec<VariantInfo>,
}

impl EnumInfo {
    /// A description of the enum `T`; the variants must be in declaration order.
    pub fn new<T: Any>(key: TypeKey, variants: Vec<VariantInfo>) -> Self {
        Self {
            header: Header::new::<T>(key),
            variants,
        }
    }

    header_methods!();

    /// The variants, in declaration order.
    pub fn variants(&self) -> &[VariantInfo] {
        &self.variants
    }

    /// The number of variants.
    pub fn variant_len(&self) -> usize {
        self.variants.len()
    }

    /// The variant with the given name.
    pub fn variant(&self, name: &str) -> Option<&VariantInfo> {
        self.variants.iter().find(|variant| variant.name == name)
    }

    /// The variant at the given position.
    pub fn variant_at(&self, index: usize) -> Option<&VariantInfo> {
        self.variants.get(index)
    }
}

/// Description of a growable sequence (`Vec<T>`).
#[derive(Clone, Debug)]
pub struct ListInfo {
    header: Header,
    item: TypeRef,
}

impl ListInfo {
    /// A description of the list `T` with the given item type.
    pub fn new<T: Any>(key: TypeKey, item: TypeRef) -> Self {
        Self {
            header: Header::new::<T>(key),
            item,
        }
    }

    header_methods!();

    /// The type of the items.
    pub fn item(&self) -> TypeRef {
        self.item
    }
}

/// Description of a fixed-size array (`[T; N]`).
#[derive(Clone, Debug)]
pub struct ArrayInfo {
    header: Header,
    item: TypeRef,
    len: usize,
}

impl ArrayInfo {
    /// A description of the array `T` with the given item type and length.
    pub fn new<T: Any>(key: TypeKey, item: TypeRef, len: usize) -> Self {
        Self {
            header: Header::new::<T>(key),
            item,
            len,
        }
    }

    header_methods!();

    /// The type of the items.
    pub fn item(&self) -> TypeRef {
        self.item
    }

    /// The number of items.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether the array has no items.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// Description of a map (`HashMap<K, V>`, `BTreeMap<K, V>`).
#[derive(Clone, Debug)]
pub struct MapInfo {
    header: Header,
    key: TypeRef,
    value: TypeRef,
}

impl MapInfo {
    /// A description of the map `T` with the given key and value types.
    pub fn new<T: Any>(key_path: TypeKey, key: TypeRef, value: TypeRef) -> Self {
        Self {
            header: Header::new::<T>(key_path),
            key,
            value,
        }
    }

    header_methods!();

    /// The type of the keys.
    pub fn key_type(&self) -> TypeRef {
        self.key
    }

    /// The type of the values.
    pub fn value_type(&self) -> TypeRef {
        self.value
    }
}

/// Description of a set (`HashSet<T>`, `BTreeSet<T>`).
#[derive(Clone, Debug)]
pub struct SetInfo {
    header: Header,
    item: TypeRef,
}

impl SetInfo {
    /// A description of the set `T` with the given item type.
    pub fn new<T: Any>(key: TypeKey, item: TypeRef) -> Self {
        Self {
            header: Header::new::<T>(key),
            item,
        }
    }

    header_methods!();

    /// The type of the items.
    pub fn item(&self) -> TypeRef {
        self.item
    }
}

/// Description of a type that is a value as far as reflection goes: numbers, `bool`, `char`, `String`,
/// `()`. Reflection does not look inside it.
#[derive(Clone, Debug)]
pub struct OpaqueInfo {
    header: Header,
}

impl OpaqueInfo {
    /// A description of the opaque type `T`.
    pub fn new<T: Any>(key: TypeKey) -> Self {
        Self {
            header: Header::new::<T>(key),
        }
    }

    header_methods!();
}

/// The static description of a reflected type.
#[derive(Clone, Debug)]
pub enum TypeInfo {
    /// A struct with named fields.
    Struct(StructInfo),
    /// A tuple struct.
    TupleStruct(TupleStructInfo),
    /// A tuple.
    Tuple(TupleInfo),
    /// An enum.
    Enum(EnumInfo),
    /// A growable sequence.
    List(ListInfo),
    /// A fixed-size array.
    Array(ArrayInfo),
    /// A map.
    Map(MapInfo),
    /// A set.
    Set(SetInfo),
    /// A value reflection does not look into.
    Opaque(OpaqueInfo),
}

impl TypeInfo {
    /// The stable key of the type.
    pub fn key(&self) -> TypeKey {
        match self {
            Self::Struct(info) => info.key(),
            Self::TupleStruct(info) => info.key(),
            Self::Tuple(info) => info.key(),
            Self::Enum(info) => info.key(),
            Self::List(info) => info.key(),
            Self::Array(info) => info.key(),
            Self::Map(info) => info.key(),
            Self::Set(info) => info.key(),
            Self::Opaque(info) => info.key(),
        }
    }

    /// The path of the type (the text the key was made from).
    pub fn path(&self) -> &'static str {
        self.key().path()
    }

    /// The 64-bit hash of the path.
    pub fn id(&self) -> u64 {
        self.key().id()
    }

    /// The other paths the type was known by; empty for most types.
    pub fn aliases(&self) -> &'static [&'static str] {
        match self {
            Self::Struct(info) => info.aliases(),
            Self::TupleStruct(info) => info.aliases(),
            Self::Tuple(info) => info.aliases(),
            Self::Enum(info) => info.aliases(),
            Self::List(info) => info.aliases(),
            Self::Array(info) => info.aliases(),
            Self::Map(info) => info.aliases(),
            Self::Set(info) => info.aliases(),
            Self::Opaque(info) => info.aliases(),
        }
    }

    /// The metadata of the type (tooltip, category, custom attributes, ...); empty for most types.
    pub fn metadata(&self) -> &Metadata {
        match self {
            Self::Struct(info) => info.metadata(),
            Self::TupleStruct(info) => info.metadata(),
            Self::Tuple(info) => info.metadata(),
            Self::Enum(info) => info.metadata(),
            Self::List(info) => info.metadata(),
            Self::Array(info) => info.metadata(),
            Self::Map(info) => info.metadata(),
            Self::Set(info) => info.metadata(),
            Self::Opaque(info) => info.metadata(),
        }
    }

    /// The standard `TypeId` of the type.
    pub fn std_type_id(&self) -> TypeId {
        match self {
            Self::Struct(info) => info.std_type_id(),
            Self::TupleStruct(info) => info.std_type_id(),
            Self::Tuple(info) => info.std_type_id(),
            Self::Enum(info) => info.std_type_id(),
            Self::List(info) => info.std_type_id(),
            Self::Array(info) => info.std_type_id(),
            Self::Map(info) => info.std_type_id(),
            Self::Set(info) => info.std_type_id(),
            Self::Opaque(info) => info.std_type_id(),
        }
    }

    /// Whether the description is the one of `T`.
    pub fn is<T: Any>(&self) -> bool {
        self.std_type_id() == TypeId::of::<T>()
    }

    /// The kind of the type.
    pub fn kind(&self) -> ReflectKind {
        match self {
            Self::Struct(_) => ReflectKind::Struct,
            Self::TupleStruct(_) => ReflectKind::TupleStruct,
            Self::Tuple(_) => ReflectKind::Tuple,
            Self::Enum(_) => ReflectKind::Enum,
            Self::List(_) => ReflectKind::List,
            Self::Array(_) => ReflectKind::Array,
            Self::Map(_) => ReflectKind::Map,
            Self::Set(_) => ReflectKind::Set,
            Self::Opaque(_) => ReflectKind::Opaque,
        }
    }

    /// The types this type is made of: the types of its fields, of the fields of its variants, its item type
    /// or its key and value types. Empty for opaque values. A type that contains itself lists itself.
    pub fn dependencies(&self) -> Vec<TypeRef> {
        match self {
            Self::Struct(info) => info.fields().iter().map(FieldInfo::ty).collect(),
            Self::TupleStruct(info) => info.fields().iter().map(FieldInfo::ty).collect(),
            Self::Tuple(info) => info.fields().iter().map(FieldInfo::ty).collect(),
            Self::Enum(info) => info
                .variants()
                .iter()
                .flat_map(|variant| variant.fields().iter().map(FieldInfo::ty))
                .collect(),
            Self::List(info) => vec![info.item()],
            Self::Array(info) => vec![info.item()],
            Self::Map(info) => vec![info.key_type(), info.value_type()],
            Self::Set(info) => vec![info.item()],
            Self::Opaque(_) => Vec::new(),
        }
    }

    /// The description as a struct, if it is one.
    pub fn as_struct(&self) -> Option<&StructInfo> {
        match self {
            Self::Struct(info) => Some(info),
            _ => None,
        }
    }

    /// The description as a tuple struct, if it is one.
    pub fn as_tuple_struct(&self) -> Option<&TupleStructInfo> {
        match self {
            Self::TupleStruct(info) => Some(info),
            _ => None,
        }
    }

    /// The description as a tuple, if it is one.
    pub fn as_tuple(&self) -> Option<&TupleInfo> {
        match self {
            Self::Tuple(info) => Some(info),
            _ => None,
        }
    }

    /// The description as an enum, if it is one.
    pub fn as_enum(&self) -> Option<&EnumInfo> {
        match self {
            Self::Enum(info) => Some(info),
            _ => None,
        }
    }

    /// The description as a list, if it is one.
    pub fn as_list(&self) -> Option<&ListInfo> {
        match self {
            Self::List(info) => Some(info),
            _ => None,
        }
    }

    /// The description as an array, if it is one.
    pub fn as_array(&self) -> Option<&ArrayInfo> {
        match self {
            Self::Array(info) => Some(info),
            _ => None,
        }
    }

    /// The description as a map, if it is one.
    pub fn as_map(&self) -> Option<&MapInfo> {
        match self {
            Self::Map(info) => Some(info),
            _ => None,
        }
    }

    /// The description as a set, if it is one.
    pub fn as_set(&self) -> Option<&SetInfo> {
        match self {
            Self::Set(info) => Some(info),
            _ => None,
        }
    }

    /// The description as an opaque value, if it is one.
    pub fn as_opaque(&self) -> Option<&OpaqueInfo> {
        match self {
            Self::Opaque(info) => Some(info),
            _ => None,
        }
    }
}
