//! R5 tests: `visit` and `ReflectVisitor`.

use std::collections::{BTreeMap, BTreeSet};
use std::convert::Infallible;

use reflection::{
    DynamicStruct, Reflect, ReflectVisitor, VisitControl, VisitNode, VisitSlot, visit,
};

#[derive(Reflect, Clone)]
struct Stats {
    health: u32,
    mana: u32,
}

#[derive(Reflect, Clone)]
struct Player {
    name: String,
    stats: Stats,
    items: Vec<String>,
    pos: (f32, f32),
    tag: Option<u8>,
    scores: BTreeMap<String, u32>,
    flags: BTreeSet<u8>,
}

fn player() -> Player {
    Player {
        name: "Ada".to_string(),
        stats: Stats { health: 10, mana: 5 },
        items: vec!["axe".to_string(), "bow".to_string()],
        pos: (1.0, 2.0),
        tag: Some(7),
        scores: BTreeMap::from([("a".to_string(), 1)]),
        flags: BTreeSet::from([5]),
    }
}

/// Records `+path` on enter and `-path` on leave (`<root>` for the root).
#[derive(Default)]
struct Recorder {
    events: Vec<String>,
}

fn label(node: &VisitNode<'_>) -> String {
    if node.path().is_empty() {
        "<root>".to_string()
    } else {
        node.path().to_string()
    }
}

impl ReflectVisitor for Recorder {
    type Error = Infallible;

    fn enter(
        &mut self,
        node: &VisitNode<'_>,
        _value: &dyn Reflect,
    ) -> Result<VisitControl, Infallible> {
        self.events.push(format!("+{}", label(node)));
        Ok(VisitControl::Continue)
    }

    fn leave(&mut self, node: &VisitNode<'_>, _value: &dyn Reflect) -> Result<(), Infallible> {
        self.events.push(format!("-{}", label(node)));
        Ok(())
    }
}

fn entered(events: &[String]) -> Vec<&str> {
    events
        .iter()
        .filter_map(|event| event.strip_prefix('+'))
        .collect()
}

#[test]
fn walks_everything_in_order() {
    let mut recorder = Recorder::default();
    visit(&player(), &mut recorder).unwrap();
    assert_eq!(
        entered(&recorder.events),
        [
            "<root>",
            "name",
            "stats",
            "stats.health",
            "stats.mana",
            "items",
            "items[0]",
            "items[1]",
            "pos",
            "pos.0",
            "pos.1",
            "tag",
            "tag.0",
            "scores",
            "scores[key 0]",
            "scores[value 0]",
            "flags",
            "flags[item 0]",
        ]
    );
}

#[test]
fn leave_comes_after_the_children() {
    let mut recorder = Recorder::default();
    visit(&Stats { health: 1, mana: 2 }, &mut recorder).unwrap();
    assert_eq!(
        recorder.events,
        [
            "+<root>",
            "+health",
            "-health",
            "+mana",
            "-mana",
            "-<root>"
        ]
    );
}

#[test]
fn depth_counts_from_the_root() {
    struct Depths(Vec<(String, usize)>);
    impl ReflectVisitor for Depths {
        type Error = Infallible;
        fn enter(
            &mut self,
            node: &VisitNode<'_>,
            _value: &dyn Reflect,
        ) -> Result<VisitControl, Infallible> {
            self.0.push((label(node), node.depth()));
            Ok(VisitControl::Continue)
        }
    }
    let mut depths = Depths(Vec::new());
    visit(&player(), &mut depths).unwrap();
    let depth_of = |path: &str| depths.0.iter().find(|(p, _)| p == path).unwrap().1;
    assert_eq!(depth_of("<root>"), 0);
    assert_eq!(depth_of("stats"), 1);
    assert_eq!(depth_of("stats.health"), 2);
    assert_eq!(depth_of("items[1]"), 2);
}

#[test]
fn skip_children_still_leaves() {
    struct SkipStats(Vec<String>);
    impl ReflectVisitor for SkipStats {
        type Error = Infallible;
        fn enter(
            &mut self,
            node: &VisitNode<'_>,
            _value: &dyn Reflect,
        ) -> Result<VisitControl, Infallible> {
            self.0.push(format!("+{}", label(node)));
            Ok(if node.path() == "stats" {
                VisitControl::SkipChildren
            } else {
                VisitControl::Continue
            })
        }
        fn leave(&mut self, node: &VisitNode<'_>, _value: &dyn Reflect) -> Result<(), Infallible> {
            self.0.push(format!("-{}", label(node)));
            Ok(())
        }
    }
    let mut visitor = SkipStats(Vec::new());
    visit(&player(), &mut visitor).unwrap();
    assert!(visitor.0.contains(&"+stats".to_string()));
    assert!(visitor.0.contains(&"-stats".to_string()));
    assert!(!visitor.0.iter().any(|event| event.contains("stats.")));
    // the siblings after it are still visited
    assert!(visitor.0.contains(&"+items".to_string()));
}

#[test]
fn stop_ends_the_walk_without_an_error() {
    struct StopAtMana(Vec<String>);
    impl ReflectVisitor for StopAtMana {
        type Error = Infallible;
        fn enter(
            &mut self,
            node: &VisitNode<'_>,
            _value: &dyn Reflect,
        ) -> Result<VisitControl, Infallible> {
            self.0.push(format!("+{}", label(node)));
            Ok(if node.path() == "stats.mana" {
                VisitControl::Stop
            } else {
                VisitControl::Continue
            })
        }
        fn leave(&mut self, node: &VisitNode<'_>, _value: &dyn Reflect) -> Result<(), Infallible> {
            self.0.push(format!("-{}", label(node)));
            Ok(())
        }
    }
    let mut visitor = StopAtMana(Vec::new());
    assert!(visit(&player(), &mut visitor).is_ok());
    assert_eq!(visitor.0.last().map(String::as_str), Some("+stats.mana"));
    assert!(!visitor.0.contains(&"+items".to_string()));
    assert!(!visitor.0.contains(&"-stats".to_string()));
}

#[test]
fn an_error_ends_the_walk_and_is_returned() {
    struct FailAtMana {
        seen: usize,
    }
    impl ReflectVisitor for FailAtMana {
        type Error = String;
        fn enter(
            &mut self,
            node: &VisitNode<'_>,
            _value: &dyn Reflect,
        ) -> Result<VisitControl, String> {
            self.seen += 1;
            if node.path() == "stats.mana" {
                Err(format!("no mana at {}", node.path()))
            } else {
                Ok(VisitControl::Continue)
            }
        }
    }
    let mut visitor = FailAtMana { seen: 0 };
    let result = visit(&player(), &mut visitor);
    assert_eq!(result, Err("no mana at stats.mana".to_string()));
    assert_eq!(visitor.seen, 5); // root, name, stats, health, mana
}

#[test]
fn works_through_a_trait_object() {
    let mut recorder = Recorder::default();
    let visitor: &mut dyn ReflectVisitor<Error = Infallible> = &mut recorder;
    visit(&Stats { health: 1, mana: 2 }, visitor).unwrap();
    assert_eq!(recorder.events.len(), 6);
}

#[derive(Reflect)]
struct Config {
    #[reflect(transient)]
    cache: u32,
    #[reflect(range(min = 0, max = 5), tooltip = "Level")]
    level: u8,
    plain: bool,
}

#[test]
fn nodes_carry_the_field_metadata() {
    struct Collect(Vec<(String, bool, Option<f64>, Option<&'static str>)>);
    impl ReflectVisitor for Collect {
        type Error = Infallible;
        fn enter(
            &mut self,
            node: &VisitNode<'_>,
            _value: &dyn Reflect,
        ) -> Result<VisitControl, Infallible> {
            if let Some(metadata) = node.metadata() {
                self.0.push((
                    label(node),
                    metadata.is_transient(),
                    metadata.range().map(|range| range.max),
                    metadata.tooltip(),
                ));
            }
            Ok(VisitControl::Continue)
        }
    }
    let mut collect = Collect(Vec::new());
    visit(
        &Config {
            cache: 1,
            level: 2,
            plain: true,
        },
        &mut collect,
    )
    .unwrap();
    assert_eq!(
        collect.0,
        [
            ("cache".to_string(), true, None, None),
            ("level".to_string(), false, Some(5.0), Some("Level")),
            ("plain".to_string(), false, None, None),
        ]
    );
}

#[derive(Reflect)]
enum Shape {
    Empty,
    Circle(f32),
    Rect {
        #[reflect(tooltip = "Width")]
        w: f32,
        h: f32,
    },
}

#[test]
fn enums_show_the_fields_of_the_active_variant() {
    let mut recorder = Recorder::default();
    visit(&Shape::Empty, &mut recorder).unwrap();
    assert_eq!(entered(&recorder.events), ["<root>"]);

    let mut recorder = Recorder::default();
    visit(&Shape::Circle(1.0), &mut recorder).unwrap();
    assert_eq!(entered(&recorder.events), ["<root>", "0"]);

    let mut recorder = Recorder::default();
    visit(&Shape::Rect { w: 1.0, h: 2.0 }, &mut recorder).unwrap();
    assert_eq!(entered(&recorder.events), ["<root>", "w", "h"]);
}

#[test]
fn enum_variant_fields_have_their_field_info() {
    struct Tooltips(Vec<(String, Option<&'static str>)>);
    impl ReflectVisitor for Tooltips {
        type Error = Infallible;
        fn enter(
            &mut self,
            node: &VisitNode<'_>,
            _value: &dyn Reflect,
        ) -> Result<VisitControl, Infallible> {
            if let Some(field) = node.field() {
                self.0.push((label(node), field.metadata().tooltip()));
            }
            Ok(VisitControl::Continue)
        }
    }
    let mut tooltips = Tooltips(Vec::new());
    visit(&Shape::Rect { w: 1.0, h: 2.0 }, &mut tooltips).unwrap();
    assert_eq!(
        tooltips.0,
        [
            ("w".to_string(), Some("Width")),
            ("h".to_string(), None)
        ]
    );
}

#[test]
fn slots_describe_where_a_value_sits() {
    struct Slots(Vec<String>);
    impl ReflectVisitor for Slots {
        type Error = Infallible;
        fn enter(
            &mut self,
            node: &VisitNode<'_>,
            value: &dyn Reflect,
        ) -> Result<VisitControl, Infallible> {
            let text = match node.slot() {
                VisitSlot::Root => "root".to_string(),
                VisitSlot::Field { name, index, info } => {
                    format!("field {name:?} #{index} info={}", info.is_some())
                }
                VisitSlot::Item(index) => format!("item {index}"),
                VisitSlot::MapKey(index) => format!("key {index}"),
                VisitSlot::MapValue { index, key } => format!(
                    "value {index} of {:?}",
                    key.downcast_ref::<String>().cloned()
                ),
            };
            let _ = value;
            self.0.push(text);
            Ok(VisitControl::Continue)
        }
    }
    let mut slots = Slots(Vec::new());
    visit(&player(), &mut slots).unwrap();
    assert!(slots.0.contains(&"root".to_string()));
    assert!(slots.0.contains(&"field Some(\"name\") #0 info=true".to_string()));
    assert!(slots.0.contains(&"item 1".to_string()));
    assert!(slots.0.contains(&"key 0".to_string()));
    assert!(slots.0.contains(&"value 0 of Some(\"a\")".to_string()));
    assert!(slots.0.contains(&"field None #1 info=true".to_string())); // pos.1
}

#[test]
fn dynamic_values_can_be_visited() {
    let mut patch = DynamicStruct::new();
    patch.insert("a", 1_u32);
    patch.insert("b", 2_u32);
    let mut recorder = Recorder::default();
    visit(&patch, &mut recorder).unwrap();
    assert_eq!(entered(&recorder.events), ["<root>", "a", "b"]);
}

#[test]
fn a_snapshot_walks_like_the_original() {
    let player = player();
    let snapshot = player.clone_value().unwrap();
    let mut original = Recorder::default();
    visit(&player, &mut original).unwrap();
    let mut copy = Recorder::default();
    visit(&*snapshot, &mut copy).unwrap();
    assert_eq!(original.events, copy.events);
}

#[test]
fn opaque_values_have_no_children() {
    let mut recorder = Recorder::default();
    visit(&42_u32, &mut recorder).unwrap();
    assert_eq!(recorder.events, ["+<root>", "-<root>"]);
}
