//! R4 tests: `Reflect::apply`, patches made of dynamic values, snapshots.

use std::collections::{BTreeMap, BTreeSet};

use reflection::{
    DynamicEnum, DynamicList, DynamicMap, DynamicSet, DynamicStruct, DynamicVariant, Reflect,
    ReflectError, ReflectKind,
};

#[derive(Reflect, Debug, PartialEq, Clone)]
struct Stats {
    health: u32,
    mana: u32,
}

#[derive(Reflect, Debug, PartialEq, Clone)]
struct Item {
    name: String,
    count: u8,
}

#[derive(Reflect, Debug, PartialEq, Clone)]
enum Slot {
    Empty,
    Held(Item),
    Named { label: String, count: u8 },
}

#[derive(Reflect, Debug, PartialEq, Clone)]
struct Player {
    name: String,
    stats: Stats,
    items: Vec<Item>,
    grid: [u8; 2],
    pos: (f32, f32),
    slot: Slot,
    boss: Option<Box<Player>>,
    tags: BTreeMap<String, u32>,
    marks: BTreeSet<u8>,
}

fn item(name: &str, count: u8) -> Item {
    Item {
        name: name.to_string(),
        count,
    }
}

fn leaf(name: &str) -> Player {
    Player {
        name: name.to_string(),
        stats: Stats { health: 1, mana: 1 },
        items: Vec::new(),
        grid: [0, 0],
        pos: (0.0, 0.0),
        slot: Slot::Empty,
        boss: None,
        tags: BTreeMap::new(),
        marks: BTreeSet::new(),
    }
}

fn base() -> Player {
    Player {
        name: "Ada".to_string(),
        stats: Stats {
            health: 10,
            mana: 5,
        },
        items: vec![item("knife", 1)],
        grid: [1, 2],
        pos: (1.0, 2.0),
        slot: Slot::Empty,
        boss: Some(Box::new(leaf("Old"))),
        tags: BTreeMap::from([("a".to_string(), 1)]),
        marks: BTreeSet::from([1]),
    }
}

fn other() -> Player {
    Player {
        name: "Grace".to_string(),
        stats: Stats {
            health: 99,
            mana: 50,
        },
        items: vec![item("sword", 2), item("shield", 3), item("bow", 4)],
        grid: [8, 9],
        pos: (3.0, 4.0),
        slot: Slot::Held(item("axe", 5)),
        boss: Some(Box::new(leaf("New"))),
        tags: BTreeMap::from([("a".to_string(), 2), ("b".to_string(), 3)]),
        marks: BTreeSet::from([1, 2]),
    }
}

#[test]
fn applies_a_value_of_the_same_type() {
    let mut target = base();
    target.apply(&other()).unwrap();
    assert_eq!(target, other());
}

#[test]
fn lists_are_resized_and_maps_and_sets_only_grow() {
    let mut numbers = vec![1_u32, 2, 3];
    numbers.apply(&vec![9_u32]).unwrap();
    assert_eq!(numbers, vec![9]);
    numbers.apply(&vec![1_u32, 2, 3, 4]).unwrap();
    assert_eq!(numbers, vec![1, 2, 3, 4]);

    let mut target = base();
    target.tags.insert("keep".to_string(), 7);
    target.marks.insert(42);
    target.apply(&other()).unwrap();
    assert_eq!(target.tags.get("keep"), Some(&7));
    assert_eq!(target.tags.get("a"), Some(&2));
    assert_eq!(target.tags.get("b"), Some(&3));
    assert!(target.marks.contains(&42));
    assert!(target.marks.contains(&2));
}

#[test]
fn a_patch_changes_only_what_it_has() {
    let mut stats = DynamicStruct::new();
    stats.insert("mana", 7_u32);
    let mut patch = DynamicStruct::new();
    patch.insert("name", "Zed".to_string());
    patch.insert("stats", stats);
    patch.insert("unknown", 1_u8);

    let mut target = base();
    target.apply(&patch).unwrap();
    let mut expected = base();
    expected.name = "Zed".to_string();
    expected.stats.mana = 7;
    assert_eq!(target, expected);
}

#[test]
fn a_patch_can_grow_a_list_with_dynamic_items() {
    let mut bow = DynamicStruct::new();
    bow.insert("name", "bow".to_string());
    bow.insert("count", 2_u8);
    let list = DynamicList::from_items(vec![Box::new(bow) as Box<dyn Reflect>]);
    let mut patch = DynamicStruct::new();
    patch.insert("items", list);

    let mut target = base();
    target.items.clear();
    target.apply(&patch).unwrap();
    assert_eq!(target.items, vec![item("bow", 2)]);
}

#[test]
fn a_patch_can_change_the_variant_of_an_enum() {
    let held = DynamicEnum::new(
        "Named",
        DynamicVariant::Struct(vec![
            ("label", Box::new("x".to_string()) as Box<dyn Reflect>),
            ("count", Box::new(1_u8) as Box<dyn Reflect>),
        ]),
    );
    let mut patch = DynamicStruct::new();
    patch.insert("slot", held);
    let mut target = base();
    target.apply(&patch).unwrap();
    assert_eq!(
        target.slot,
        Slot::Named {
            label: "x".to_string(),
            count: 1
        }
    );

    // The same variant is changed field by field.
    let same = DynamicEnum::new(
        "Named",
        DynamicVariant::Struct(vec![("count", Box::new(9_u8) as Box<dyn Reflect>)]),
    );
    let mut patch = DynamicStruct::new();
    patch.insert("slot", same);
    target.apply(&patch).unwrap();
    assert_eq!(
        target.slot,
        Slot::Named {
            label: "x".to_string(),
            count: 9
        }
    );

    // `None` becomes `Some` through the same rebuilding.
    let mut target = base();
    target.boss = None;
    target.apply(&other()).unwrap();
    assert_eq!(target.boss, other().boss);
}

#[test]
fn a_patch_can_add_map_entries_and_set_items() {
    let tags = DynamicMap::from_entries(vec![
        (
            Box::new("a".to_string()) as Box<dyn Reflect>,
            Box::new(9_u32) as Box<dyn Reflect>,
        ),
        (
            Box::new("z".to_string()) as Box<dyn Reflect>,
            Box::new(1_u32) as Box<dyn Reflect>,
        ),
    ]);
    let marks = DynamicSet::from_items(vec![
        Box::new(1_u8) as Box<dyn Reflect>,
        Box::new(5_u8) as Box<dyn Reflect>,
    ]);
    let mut patch = DynamicStruct::new();
    patch.insert("tags", tags);
    patch.insert("marks", marks);
    let mut target = base();
    target.apply(&patch).unwrap();
    assert_eq!(
        target.tags,
        BTreeMap::from([("a".to_string(), 9), ("z".to_string(), 1)])
    );
    assert_eq!(target.marks, BTreeSet::from([1, 5]));
}

#[test]
fn reports_what_cannot_be_applied() {
    let mut stats = Stats { health: 1, mana: 1 };
    assert_eq!(
        stats.apply(&vec![1_u8]).err(),
        Some(ReflectError::KindMismatch {
            expected: ReflectKind::Struct,
            actual: ReflectKind::List
        })
    );
    let mut grid = [1_u8, 2];
    assert_eq!(
        grid.apply(&[1_u8, 2, 3]).err(),
        Some(ReflectError::LengthMismatch {
            expected: 2,
            actual: 3
        })
    );
    let mut number = 1_u32;
    assert_eq!(
        number.apply(&"x".to_string()).err(),
        Some(ReflectError::type_mismatch("u32", "String"))
    );
    number.apply(&7_u32).unwrap();
    assert_eq!(number, 7);
}

#[test]
fn a_snapshot_restores_the_value() {
    let mut player = other();
    let snapshot = player.clone_value().unwrap();

    player.name = "changed".to_string();
    player.stats.health = 1;
    player.items.clear();
    player.slot = Slot::Empty;
    player.boss = None;
    player.tags.insert("new".to_string(), 5);
    player.marks.insert(99);

    // `apply` puts back everything except what the maps and sets gained.
    let mut applied = player.clone();
    applied.apply(&*snapshot).unwrap();
    assert_eq!(applied.name, "Grace");
    assert_eq!(applied.stats, other().stats);
    assert_eq!(applied.items, other().items);
    assert_eq!(applied.slot, other().slot);
    assert_eq!(applied.boss, other().boss);
    assert_eq!(applied.tags.get("new"), Some(&5));
    assert!(applied.marks.contains(&99));

    // `try_replace` rebuilds the whole value and restores it exactly.
    player.try_replace(&*snapshot).unwrap();
    assert_eq!(player, other());
}
