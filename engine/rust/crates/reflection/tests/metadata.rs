//! R5 tests: metadata attributes (`tooltip`, `category`, `range`, `readonly`, `hidden`, `transient`,
//! `default`, typed custom attributes, doc comments) on types, fields and variants.

use reflection::{DynamicStruct, GetPath, Reflect, Typed, ValueRange};

struct Slider {
    step: f32,
}

struct Group(&'static str);

#[derive(Reflect)]
#[reflect(category = "Gameplay", tooltip = "A hero", transient, attr = Group("heroes"))]
struct Hero {
    #[reflect(tooltip = "Hit points", range(min = 0.0, max = 100.0), attr = Slider { step: 0.5 })]
    hp: f32,
    #[reflect(readonly, hidden)]
    id: u64,
    #[reflect(default, category = "Stats")]
    mana: u32,
    plain: bool,
}

#[derive(Reflect)]
struct Pair(#[reflect(tooltip = "left")] u8, u8);

#[derive(Reflect)]
enum State {
    #[reflect(tooltip = "Waiting", category = "Idle", hidden, attr = Group("states"))]
    Idle,
    Moving {
        #[reflect(range(min = 0, max = 10))]
        speed: f32,
    },
    Carry(#[reflect(tooltip = "Item")] u32),
}

#[derive(Reflect)]
struct Wrapper<T> {
    #[reflect(tooltip = "inner")]
    value: T,
}

fn hero() -> Hero {
    Hero {
        hp: 10.0,
        id: 1,
        mana: 2,
        plain: true,
    }
}

#[test]
fn type_metadata_is_read_from_the_description() {
    let metadata = Hero::static_type_info().metadata();
    assert_eq!(metadata.category(), Some("Gameplay"));
    assert_eq!(metadata.tooltip(), Some("A hero"));
    assert!(metadata.is_transient());
    assert!(!metadata.is_hidden());
    assert!(!metadata.is_readonly());
    assert_eq!(
        metadata.custom::<Group>().map(|group| group.0),
        Some("heroes")
    );
    assert_eq!(metadata.custom_len(), 1);
}

#[test]
fn field_metadata_is_read_from_the_description() {
    let info = Hero::static_type_info().as_struct().unwrap();

    let hp = info.field("hp").unwrap().metadata();
    assert_eq!(hp.tooltip(), Some("Hit points"));
    assert_eq!(
        hp.range(),
        Some(ValueRange {
            min: 0.0,
            max: 100.0
        })
    );
    assert_eq!(hp.custom::<Slider>().map(|slider| slider.step), Some(0.5));
    assert!(!hp.is_readonly());

    let id = info.field("id").unwrap().metadata();
    assert!(id.is_readonly());
    assert!(id.is_hidden());
    assert!(!id.is_transient());

    let mana = info.field("mana").unwrap().metadata();
    assert!(mana.has_default());
    assert_eq!(mana.category(), Some("Stats"));

    assert!(info.field("plain").unwrap().metadata().is_empty());
}

#[test]
fn types_without_attributes_have_empty_metadata() {
    assert!(u32::static_type_info().metadata().is_empty());
    assert!(<Vec<u32>>::static_type_info().metadata().is_empty());
    assert!(<(u8, u8)>::static_type_info().metadata().is_empty());
    assert!(<Option<String>>::static_type_info().metadata().is_empty());
    assert!(DynamicStruct::new().type_info().metadata().is_empty());
}

#[test]
fn tuple_struct_fields_have_metadata() {
    let info = Pair::static_type_info().as_tuple_struct().unwrap();
    assert_eq!(info.field_at(0).unwrap().metadata().tooltip(), Some("left"));
    assert!(info.field_at(1).unwrap().metadata().is_empty());
}

#[test]
fn variants_and_their_fields_have_metadata() {
    let info = State::static_type_info().as_enum().unwrap();

    let idle = info.variant("Idle").unwrap().metadata();
    assert_eq!(idle.tooltip(), Some("Waiting"));
    assert_eq!(idle.category(), Some("Idle"));
    assert!(idle.is_hidden());
    assert_eq!(idle.custom::<Group>().map(|group| group.0), Some("states"));

    let moving = info.variant("Moving").unwrap();
    assert!(moving.metadata().is_empty());
    let speed = moving.field("speed").unwrap().metadata();
    assert_eq!(
        speed.range(),
        Some(ValueRange {
            min: 0.0,
            max: 10.0
        })
    );

    let carry = info.variant("Carry").unwrap();
    assert_eq!(
        carry.field_at(0).unwrap().metadata().tooltip(),
        Some("Item")
    );
}

#[test]
fn generic_types_keep_the_metadata_of_their_fields() {
    let info = Wrapper::<u32>::static_type_info().as_struct().unwrap();
    assert_eq!(
        info.field("value").unwrap().metadata().tooltip(),
        Some("inner")
    );
    let info = Wrapper::<String>::static_type_info().as_struct().unwrap();
    assert_eq!(
        info.field("value").unwrap().metadata().tooltip(),
        Some("inner")
    );
}

#[test]
fn metadata_is_information_only() {
    // A read-only, transient field can still be written through reflection.
    let mut hero = hero();
    *hero.get_path_mut::<u64>("id").unwrap() = 99;
    assert_eq!(hero.id, 99);
    // `range` is not enforced either.
    *hero.get_path_mut::<f32>("hp").unwrap() = 1000.0;
    assert!((hero.hp - 1000.0).abs() < f32::EPSILON);
    // Reading the fields keeps the rest of the struct intact.
    assert_eq!(hero.mana, 2);
    assert!(hero.plain);
}

#[test]
fn value_range_helpers() {
    let range = Hero::static_type_info()
        .as_struct()
        .unwrap()
        .field("hp")
        .unwrap()
        .metadata()
        .range()
        .unwrap();
    assert!(range.contains(50.0));
    assert!(!range.contains(150.0));
    assert!((range.clamp(150.0) - 100.0).abs() < f64::EPSILON);
    assert!((range.clamp(-5.0) - 0.0).abs() < f64::EPSILON);
}

#[test]
fn metadata_of_the_description_and_of_the_value_agree() {
    let hero = hero();
    let from_value = hero.type_info().metadata();
    let from_type = Hero::static_type_info().metadata();
    assert_eq!(from_value.category(), from_type.category());
    assert_eq!(from_value.custom_len(), from_type.custom_len());
}

/// A documented struct.
///
/// With a second paragraph.
#[derive(Reflect)]
struct Documented {
    /// The first field.
    first: u32,
    second: u32,
}

#[derive(Reflect)]
enum DocumentedEnum {
    /// The only variant.
    Only,
}

#[cfg(feature = "docs")]
#[test]
fn doc_comments_are_captured() {
    let info = Documented::static_type_info();
    assert_eq!(
        info.metadata().docs(),
        Some("A documented struct.\n\nWith a second paragraph.")
    );
    let info = info.as_struct().unwrap();
    assert_eq!(
        info.field("first").unwrap().metadata().docs(),
        Some("The first field.")
    );
    assert_eq!(info.field("second").unwrap().metadata().docs(), None);
    let variants = DocumentedEnum::static_type_info().as_enum().unwrap();
    assert_eq!(
        variants.variant("Only").unwrap().metadata().docs(),
        Some("The only variant.")
    );
}

#[cfg(not(feature = "docs"))]
#[test]
fn doc_comments_are_not_captured_without_the_docs_feature() {
    let info = Documented::static_type_info();
    assert_eq!(info.metadata().docs(), None);
    let info = info.as_struct().unwrap();
    assert_eq!(info.field("first").unwrap().metadata().docs(), None);
    let variants = DocumentedEnum::static_type_info().as_enum().unwrap();
    assert_eq!(variants.variant("Only").unwrap().metadata().docs(), None);
}
