//! R3 tests: the type registry, `inventory` collection and type data.

use std::any::TypeId;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use reflection::{
    FromType, Reflect, ReflectClone, ReflectDebug, ReflectDefault, ReflectError, ReflectHash,
    ReflectPartialEq, TypeKey, TypeRegistry, Typed,
};

#[derive(Reflect, Debug, Clone, PartialEq, Eq, Hash, Default)]
#[reflect(
    name = "app::Config",
    alias = "app::OldConfig",
    Default,
    Clone,
    PartialEq,
    Debug,
    Hash
)]
struct Config {
    width: u32,
    title: String,
}

#[derive(Reflect, Debug, Clone, PartialEq, Default)]
#[reflect(Default)]
struct Tag {
    label: String,
}

#[derive(Reflect, Debug, PartialEq)]
struct NoDefault {
    value: u8,
}

#[derive(Reflect, Default)]
struct Wrapper<T> {
    inner: Vec<T>,
}

#[derive(Reflect)]
struct Holder {
    items: Vec<Config>,
}

struct ReflectGreeting {
    text: &'static str,
}

impl<T: Typed> FromType<T> for ReflectGreeting {
    fn from_type() -> Self {
        Self { text: "hello" }
    }
}

#[derive(Reflect)]
#[reflect(ReflectGreeting)]
struct Greeter;

fn config() -> Config {
    Config {
        width: 640,
        title: "hi".to_string(),
    }
}

#[test]
fn registering_registers_what_a_type_is_made_of() {
    let mut registry = TypeRegistry::new();
    assert!(registry.is_empty());
    registry.register::<Wrapper<Tag>>().unwrap();
    // Wrapper<Tag>, Vec<Tag>, Tag and the String inside Tag.
    assert_eq!(registry.len(), 4);
    assert!(registry.contains::<Wrapper<Tag>>());
    assert!(registry.contains::<Vec<Tag>>());
    assert!(registry.contains::<Tag>());
    assert!(registry.contains::<String>());
    assert!(!registry.contains::<Config>());
    assert_eq!(
        registry.get_by_type::<Wrapper<Tag>>().unwrap().path(),
        "registry::Wrapper<registry::Tag>"
    );

    let tag = registry.get_by_type::<Tag>().unwrap();
    assert_eq!(tag.path(), "registry::Tag");
    assert!(tag.has_data::<ReflectDefault>());
    assert!(!tag.has_data::<ReflectClone>());
    assert!(
        registry
            .get_by_type::<Vec<Tag>>()
            .unwrap()
            .has_data::<ReflectDefault>()
    );

    registry.register::<Wrapper<Tag>>().unwrap();
    registry.register::<Tag>().unwrap();
    assert_eq!(registry.len(), 4);
    assert_eq!(registry.iter().count(), 4);
}

#[test]
fn types_are_found_by_every_name() {
    let mut registry = TypeRegistry::new();
    registry.register::<Config>().unwrap();
    for name in ["app::Config", "app::OldConfig"] {
        let registration = registry.get_by_path(name).unwrap();
        assert!(registration.info().is::<Config>());
    }
    assert!(registry.get_by_path("app::Missing").is_none());
    let key = TypeKey::new("app::Config");
    assert!(registry.get_by_key(&key).unwrap().info().is::<Config>());
    assert!(registry.get_by_id(key.id()).is_some());
    assert!(registry.get_by_id(1).is_none());
    assert!(registry.get(TypeId::of::<Config>()).is_some());
    assert!(registry.get(TypeId::of::<Tag>()).is_none());
    assert_eq!(
        registry.get_by_type::<Config>().unwrap().info().aliases(),
        &["app::OldConfig"]
    );
}

#[test]
fn standard_traits_through_type_data() {
    let mut registry = TypeRegistry::new();
    registry.register::<Config>().unwrap();
    let config = config();

    let default = registry.create_default("app::OldConfig").unwrap();
    assert_eq!(default.downcast_ref::<Config>(), Some(&Config::default()));

    let cloned = registry.clone_value(&config).unwrap();
    assert_eq!(cloned.downcast_ref::<Config>(), Some(&config));

    let eq = registry.data::<Config, ReflectPartialEq>().unwrap();
    assert_eq!(eq.eq(&config, &config.clone()), Ok(true));
    assert_eq!(eq.eq(&config, &Config::default()), Ok(false));
    assert_eq!(
        eq.eq(&config, &5_u32),
        Err(ReflectError::type_mismatch("app::Config", "u32"))
    );

    let debug = registry.data::<Config, ReflectDebug>().unwrap();
    assert_eq!(format!("{:?}", debug.debug(&config)), format!("{config:?}"));
    assert_eq!(format!("{:?}", debug.debug(&5_u32)), "<type mismatch>");

    let hash = registry.data::<Config, ReflectHash>().unwrap();
    let mut expected = DefaultHasher::new();
    config.hash(&mut expected);
    let mut actual = DefaultHasher::new();
    hash.hash_value(&config, &mut actual).unwrap();
    assert_eq!(expected.finish(), actual.finish());
    assert!(hash.hash_value(&5_u32, &mut actual).is_err());
}

#[test]
fn lookups_and_helpers_report_what_is_missing() {
    let mut registry = TypeRegistry::new();
    assert_eq!(
        registry.create_default("nope").err(),
        Some(ReflectError::UnknownType {
            name: "nope".to_string()
        })
    );
    registry.register::<NoDefault>().unwrap();
    assert_eq!(
        registry.create_default("registry::NoDefault").err(),
        Some(ReflectError::MissingTypeData {
            path: "registry::NoDefault",
            data: "ReflectDefault"
        })
    );
    assert_eq!(
        registry.clone_value(&NoDefault { value: 1 }).err(),
        Some(ReflectError::MissingTypeData {
            path: "registry::NoDefault",
            data: "ReflectClone"
        })
    );
    assert_eq!(
        registry.clone_value(&Greeter).err(),
        Some(ReflectError::UnknownType {
            name: "registry::Greeter".to_string()
        })
    );
}

#[test]
fn standard_types_come_with_type_data() {
    let mut registry = TypeRegistry::new();
    registry.register::<u32>().unwrap();
    registry.register::<Vec<String>>().unwrap();
    registry.register::<Option<u8>>().unwrap();
    registry.register::<f32>().unwrap();

    let zero = registry.create_default("u32").unwrap();
    assert_eq!(zero.downcast_ref::<u32>(), Some(&0));
    let empty = registry.create_default("Vec<String>").unwrap();
    assert!(empty.downcast_ref::<Vec<String>>().unwrap().is_empty());
    let none = registry.create_default("Option<u8>").unwrap();
    assert_eq!(none.downcast_ref::<Option<u8>>(), Some(&None));

    let float = registry.get_by_type::<f32>().unwrap();
    assert!(float.has_data::<ReflectPartialEq>());
    assert!(float.has_data::<ReflectClone>());
    assert!(!float.has_data::<ReflectHash>());
    let integer = registry.get_by_type::<u32>().unwrap();
    assert!(integer.has_data::<ReflectHash>());
    // ReflectFromReflect plus the five standard traits.
    assert_eq!(integer.data_len(), 6);
}

#[test]
fn custom_type_data_through_the_derive_attribute() {
    let mut registry = TypeRegistry::new();
    registry.register::<Greeter>().unwrap();
    assert_eq!(
        registry.data::<Greeter, ReflectGreeting>().unwrap().text,
        "hello"
    );
}

#[test]
fn type_data_can_be_added_by_hand() {
    let mut registry = TypeRegistry::new();
    registry
        .add_type_data::<Wrapper<u8>, ReflectDefault>(
            <ReflectDefault as FromType<Wrapper<u8>>>::from_type(),
        )
        .unwrap();
    assert!(registry.contains::<Vec<u8>>());
    let value = registry.create_default("registry::Wrapper<u8>").unwrap();
    assert!(
        value
            .downcast_ref::<Wrapper<u8>>()
            .unwrap()
            .inner
            .is_empty()
    );

    let registration = registry.get_by_type::<Wrapper<u8>>().unwrap();
    assert!(registration.has_data::<ReflectDefault>());
    let mut registration = reflection::TypeRegistration::of::<Tag>();
    assert!(registration.has_data::<ReflectDefault>());
    assert!(registration.remove_data::<ReflectDefault>());
    assert!(!registration.remove_data::<ReflectDefault>());
    // Only the ReflectFromReflect every registration has is left.
    assert_eq!(registration.data_len(), 1);
}

#[test]
fn derived_types_are_collected_from_inventory() {
    let registry = TypeRegistry::from_inventory().unwrap();
    assert!(registry.contains::<Config>());
    assert!(registry.contains::<Tag>());
    assert!(registry.contains::<NoDefault>());
    assert!(registry.contains::<Greeter>());
    assert!(registry.contains::<Holder>());
    // Found through the fields of `Holder` and `Config`.
    assert!(registry.contains::<Vec<Config>>());
    assert!(registry.contains::<String>());
    assert!(registry.contains::<u32>());
    // Generic instantiations are not submitted by the derive.
    assert!(!registry.contains::<Wrapper<Tag>>());
    assert!(
        registry
            .get_by_path("app::OldConfig")
            .unwrap()
            .info()
            .is::<Config>()
    );
    assert!(
        registry
            .get_by_type::<Config>()
            .unwrap()
            .has_data::<ReflectDefault>()
    );
    let holder = Holder { items: Vec::new() };
    assert!(holder.items.is_empty());
}
