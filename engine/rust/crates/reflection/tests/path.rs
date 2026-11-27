//! R4 tests: paths into reflected values.

use reflection::{GetPath, Reflect, ReflectError, ReflectPath};

#[derive(Reflect, Debug, PartialEq, Clone)]
struct Stats {
    health: u32,
    ratio: f32,
}

#[derive(Reflect, Debug, PartialEq, Clone)]
struct Item(String, u8);

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
    grid: [u8; 3],
    pos: (f32, f32),
    slot: Slot,
    boss: Option<Box<Player>>,
}

fn minimal(name: &str) -> Player {
    Player {
        name: name.to_string(),
        stats: Stats {
            health: 1,
            ratio: 1.0,
        },
        items: Vec::new(),
        grid: [0, 0, 0],
        pos: (0.0, 0.0),
        slot: Slot::Empty,
        boss: None,
    }
}

fn player() -> Player {
    Player {
        name: "Ada".to_string(),
        stats: Stats {
            health: 100,
            ratio: 0.5,
        },
        items: vec![Item("sword".to_string(), 1), Item("shield".to_string(), 2)],
        grid: [7, 8, 9],
        pos: (1.0, 2.0),
        slot: Slot::Held(Item("axe".to_string(), 3)),
        boss: Some(Box::new(minimal("Boss"))),
    }
}

fn mismatch(path: &str, segment: usize, reason: &'static str) -> ReflectError {
    ReflectError::PathMismatch {
        path: path.to_string(),
        segment,
        reason,
    }
}

#[test]
fn reads_values_by_path() {
    let player = player();
    assert_eq!(player.get_path::<String>("name").unwrap(), "Ada");
    assert_eq!(player.get_path::<u32>("stats.health"), Ok(&100));
    assert_eq!(player.get_path::<String>("items[1].0").unwrap(), "shield");
    assert_eq!(player.get_path::<u8>("items[0].1"), Ok(&1));
    assert_eq!(player.get_path::<u8>("grid[2]"), Ok(&9));
    assert_eq!(player.get_path::<f32>("pos.1"), Ok(&2.0));
    // `Held(Item)`: `.0` is the item, the second `.0` its first field.
    assert_eq!(player.get_path::<String>("slot.0.0").unwrap(), "axe");
    // `Option<Box<Player>>`: `.0` is the box, the second `.0` the player inside.
    assert_eq!(player.get_path::<String>("boss.0.0.name").unwrap(), "Boss");
    assert!(player.reflect_path("").unwrap().is::<Player>());

    let mut named = player.clone();
    named.slot = Slot::Named {
        label: "gem".to_string(),
        count: 4,
    };
    assert_eq!(named.get_path::<String>("slot.label").unwrap(), "gem");
    assert_eq!(named.get_path::<u8>("slot.count"), Ok(&4));
}

#[test]
fn explains_what_does_not_fit() {
    let mut player = player();
    assert_eq!(
        player.get_path::<u32>("stats.nope").err(),
        Some(mismatch("stats.nope", 1, "no field with that name"))
    );
    assert_eq!(
        player.reflect_path("items[9]").err(),
        Some(mismatch("items[9]", 1, "index out of bounds"))
    );
    assert_eq!(
        player.reflect_path("name.x").err(),
        Some(mismatch(
            "name.x",
            1,
            "the segment does not fit the kind of the value"
        ))
    );
    assert_eq!(
        player.reflect_path("pos.5").err(),
        Some(mismatch("pos.5", 1, "no field at that index"))
    );
    assert_eq!(
        player.get_path::<u8>("stats.health").err(),
        Some(ReflectError::type_mismatch("u8", "u32"))
    );
    assert!(matches!(
        player.reflect_path("a[").err(),
        Some(ReflectError::InvalidPath { .. })
    ));
    player.slot = Slot::Empty;
    assert_eq!(
        player.reflect_path("slot.0").err(),
        Some(mismatch("slot.0", 1, "no field at that index"))
    );
}

#[test]
fn changes_values_by_path() {
    let mut player = player();
    *player.get_path_mut::<u32>("stats.health").unwrap() = 5;
    player
        .get_path_mut::<String>("items[0].0")
        .unwrap()
        .push('!');
    player
        .set_path("name", Box::new("Zed".to_string()))
        .unwrap();
    player.set_path("grid[0]", Box::new(1_u8)).unwrap();
    player.set_path("slot", Box::new(Slot::Empty)).unwrap();
    assert_eq!(player.stats.health, 5);
    assert_eq!(player.items[0].0, "sword!");
    assert_eq!(player.name, "Zed");
    assert_eq!(player.grid, [1, 8, 9]);
    assert_eq!(player.slot, Slot::Empty);
    assert_eq!(
        player
            .set_path("stats.health", Box::new("x".to_string()))
            .err(),
        Some(ReflectError::type_mismatch("u32", "String"))
    );
    assert_eq!(
        player.get_path_mut::<u8>("stats.health").err(),
        Some(ReflectError::type_mismatch("u8", "u32"))
    );
    assert_eq!(player.stats.health, 5);
}

#[test]
fn works_on_dyn_values_and_parsed_paths() {
    let mut player = player();
    {
        let value: &dyn Reflect = &player;
        assert_eq!(value.get_path::<u32>("stats.health"), Ok(&100));
    }
    let boxed: Box<dyn Reflect> = Box::new(player.clone());
    assert_eq!(boxed.get_path::<u8>("grid[1]"), Ok(&8));

    let path = ReflectPath::parse("stats.health").unwrap();
    assert_eq!(
        path.reflect(&player).unwrap().downcast_ref::<u32>(),
        Some(&100)
    );
    *path
        .reflect_mut(&mut player)
        .unwrap()
        .downcast_mut::<u32>()
        .unwrap() = 1;
    assert_eq!(player.stats.health, 1);
    assert_eq!(
        ReflectPath::parse("nope").unwrap().reflect(&player).err(),
        Some(mismatch("nope", 0, "no field with that name"))
    );
}
