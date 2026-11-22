//! R2 tests: `#[derive(Reflect)]`.

use std::collections::HashMap;

use reflection::{Reflect, ReflectKind, ReflectMut, ReflectRef, TypeKey, Typed, VariantKind};

#[derive(Debug, PartialEq, Clone, Default)]
struct Cache(u8);

#[derive(Reflect, Debug, PartialEq, Clone, Default)]
struct Player {
    name: String,
    #[reflect(rename = "hp", alias = "health")]
    health: u32,
    position: (f32, f32),
    tags: Vec<String>,
    weapon: Option<String>,
    #[reflect(skip)]
    cache: Cache,
}

fn player() -> Player {
    Player {
        name: "Ada".to_string(),
        health: 100,
        position: (1.0, 2.0),
        tags: vec!["mage".to_string()],
        weapon: None,
        cache: Cache(7),
    }
}

#[test]
fn struct_fields_are_reachable_by_name_alias_and_index() {
    let player = player();
    let value: &dyn Reflect = &player;
    let ReflectRef::Struct(view) = value.reflect_ref() else {
        panic!("a struct is expected");
    };
    assert_eq!(view.field_len(), 5);
    assert_eq!(view.name_at(0), Some("name"));
    assert_eq!(view.name_at(1), Some("hp"));
    assert_eq!(view.name_at(5), None);
    assert_eq!(view.field("hp").unwrap().downcast_ref::<u32>(), Some(&100));
    assert_eq!(
        view.field("health").unwrap().downcast_ref::<u32>(),
        Some(&100)
    );
    assert_eq!(
        view.field_at(0)
            .unwrap()
            .downcast_ref::<String>()
            .map(String::as_str),
        Some("Ada")
    );
    assert!(view.field("cache").is_none());
    assert!(view.field("missing").is_none());
    assert!(view.field_at(5).is_none());
}

#[test]
fn struct_description() {
    let info = Player::static_type_info();
    assert_eq!(info.kind(), ReflectKind::Struct);
    assert_eq!(info.path(), "derive::Player");
    assert!(info.is::<Player>());
    assert!(info.aliases().is_empty());
    let info = info.as_struct().unwrap();
    assert_eq!(info.field_len(), 5);
    assert!(info.field("cache").is_none());
    assert!(info.field("health").is_none());
    let hp = info.field("hp").unwrap();
    assert_eq!(hp.aliases(), &["health"]);
    assert_eq!(hp.index(), 1);
    assert!(hp.ty().is::<u32>());
    assert_eq!(
        info.field("position").unwrap().ty().info().path(),
        "(f32,f32)"
    );
    assert_eq!(
        info.field("weapon").unwrap().ty().info().path(),
        "Option<String>"
    );
}

#[test]
fn struct_fields_can_be_written() {
    let mut player = player();
    {
        let value: &mut dyn Reflect = &mut player;
        let ReflectMut::Struct(view) = value.reflect_mut() else {
            panic!("a struct is expected");
        };
        *view
            .field_mut("health")
            .unwrap()
            .downcast_mut::<u32>()
            .unwrap() = 40;
        view.field_at_mut(0)
            .unwrap()
            .set(Box::new("Grace".to_string()))
            .unwrap();
        assert!(view.field_mut("cache").is_none());
    }
    assert_eq!(player.health, 40);
    assert_eq!(player.name, "Grace");
    assert_eq!(player.cache, Cache(7));
}

#[test]
fn whole_value_set_and_take() {
    let mut target = Player::default();
    target.set(Box::new(player())).unwrap();
    assert_eq!(target, player());
    let boxed: Box<dyn Reflect> = Box::new(player());
    assert_eq!(boxed.take::<Player>(), Ok(player()));
    let mismatch = target.set(Box::new(5_u32)).unwrap_err();
    assert_eq!(
        mismatch.to_string(),
        "type mismatch: expected `derive::Player`, got `u32`"
    );
}

#[derive(Reflect, Debug, PartialEq)]
struct Meters(f32);

#[derive(Reflect, Debug, PartialEq)]
struct Mixed(u32, #[reflect(skip)] Cache, String);

#[derive(Reflect, Debug, PartialEq)]
struct Marker;

#[test]
fn tuple_and_unit_structs() {
    let mut meters = Meters(1.5);
    {
        let value: &mut dyn Reflect = &mut meters;
        assert_eq!(value.kind(), ReflectKind::TupleStruct);
        let ReflectMut::TupleStruct(view) = value.reflect_mut() else {
            panic!("a tuple struct is expected");
        };
        assert_eq!(view.field_len(), 1);
        *view.field_mut(0).unwrap().downcast_mut::<f32>().unwrap() = 2.5;
        assert!(view.field(1).is_none());
    }
    assert_eq!(meters, Meters(2.5));
    assert_eq!(Meters::static_type_info().path(), "derive::Meters");

    let mixed = Mixed(1, Cache(2), "x".to_string());
    let value: &dyn Reflect = &mixed;
    let ReflectRef::TupleStruct(view) = value.reflect_ref() else {
        panic!("a tuple struct is expected");
    };
    assert_eq!(view.field_len(), 2);
    assert_eq!(view.field(0).unwrap().downcast_ref::<u32>(), Some(&1));
    assert_eq!(
        view.field(1)
            .unwrap()
            .downcast_ref::<String>()
            .map(String::as_str),
        Some("x")
    );
    assert!(view.field(2).is_none());
    let info = Mixed::static_type_info().as_tuple_struct().unwrap();
    assert_eq!(info.field_len(), 2);
    assert_eq!(info.field_at(1).unwrap().index(), 1);

    let marker = Marker;
    let value: &dyn Reflect = &marker;
    assert_eq!(value.kind(), ReflectKind::Struct);
    let ReflectRef::Struct(view) = value.reflect_ref() else {
        panic!("a struct is expected");
    };
    assert_eq!(view.field_len(), 0);
    assert!(view.field("x").is_none());
    assert!(view.field_at(0).is_none());
    assert!(view.name_at(0).is_none());
}

#[derive(Reflect, Debug, PartialEq)]
enum Shape {
    Empty,
    Circle(f32),
    Rect {
        w: f32,
        #[reflect(alias = "height")]
        h: f32,
        #[reflect(skip)]
        note: u8,
    },
    #[reflect(rename = "Gone", alias = "Removed")]
    Deleted,
}

#[test]
fn enum_variants_and_fields() {
    let empty = Shape::Empty;
    let value: &dyn Reflect = &empty;
    assert_eq!(value.kind(), ReflectKind::Enum);
    let ReflectRef::Enum(view) = value.reflect_ref() else {
        panic!("an enum is expected");
    };
    assert_eq!(view.variant_name(), "Empty");
    assert_eq!(view.variant_index(), 0);
    assert_eq!(view.variant_kind(), VariantKind::Unit);
    assert_eq!(view.field_len(), 0);
    assert!(view.field_at(0).is_none());

    let circle = Shape::Circle(1.0);
    let value: &dyn Reflect = &circle;
    let ReflectRef::Enum(view) = value.reflect_ref() else {
        panic!("an enum is expected");
    };
    assert_eq!(view.variant_name(), "Circle");
    assert_eq!(view.variant_kind(), VariantKind::Tuple);
    assert_eq!(view.field_len(), 1);
    assert_eq!(view.field_at(0).unwrap().downcast_ref::<f32>(), Some(&1.0));
    assert!(view.field_at(1).is_none());
    assert!(view.field("x").is_none());
    assert!(view.name_at(0).is_none());

    let rect = Shape::Rect {
        w: 1.0,
        h: 2.0,
        note: 9,
    };
    let value: &dyn Reflect = &rect;
    let ReflectRef::Enum(view) = value.reflect_ref() else {
        panic!("an enum is expected");
    };
    assert_eq!(view.variant_name(), "Rect");
    assert_eq!(view.variant_index(), 2);
    assert_eq!(view.variant_kind(), VariantKind::Struct);
    assert_eq!(view.field_len(), 2);
    assert_eq!(view.name_at(1), Some("h"));
    assert_eq!(
        view.field("height").unwrap().downcast_ref::<f32>(),
        Some(&2.0)
    );
    assert_eq!(view.field("h").unwrap().downcast_ref::<f32>(), Some(&2.0));
    assert!(view.field("note").is_none());

    let deleted = Shape::Deleted;
    let value: &dyn Reflect = &deleted;
    let ReflectRef::Enum(view) = value.reflect_ref() else {
        panic!("an enum is expected");
    };
    assert_eq!(view.variant_name(), "Gone");
    assert_eq!(view.variant_index(), 3);
}

#[test]
fn enum_fields_can_be_written() {
    let mut shape = Shape::Rect {
        w: 1.0,
        h: 2.0,
        note: 9,
    };
    {
        let value: &mut dyn Reflect = &mut shape;
        let ReflectMut::Enum(view) = value.reflect_mut() else {
            panic!("an enum is expected");
        };
        *view.field_mut("w").unwrap().downcast_mut::<f32>().unwrap() = 5.0;
        *view.field_at_mut(1).unwrap().downcast_mut::<f32>().unwrap() = 7.0;
        assert!(view.field_at_mut(2).is_none());
    }
    assert_eq!(
        shape,
        Shape::Rect {
            w: 5.0,
            h: 7.0,
            note: 9
        }
    );
    shape.set(Box::new(Shape::Circle(3.0))).unwrap();
    assert_eq!(shape, Shape::Circle(3.0));
}

#[test]
fn enum_description() {
    let info = Shape::static_type_info().as_enum().unwrap();
    assert_eq!(info.variant_len(), 4);
    assert!(info.variant("Deleted").is_none());
    let gone = info.variant("Gone").unwrap();
    assert_eq!(gone.aliases(), &["Removed"]);
    assert_eq!(gone.index(), 3);
    let rect = info.variant("Rect").unwrap();
    assert_eq!(rect.kind(), VariantKind::Struct);
    assert_eq!(rect.field_len(), 2);
    assert_eq!(rect.field("h").unwrap().aliases(), &["height"]);
    assert_eq!(info.variant_at(1).unwrap().kind(), VariantKind::Tuple);
    assert_eq!(info.variant_at(0).unwrap().kind(), VariantKind::Unit);
    assert_eq!(
        info.variant_at(1)
            .unwrap()
            .field_at(0)
            .unwrap()
            .ty()
            .info()
            .path(),
        "f32"
    );
}

#[derive(Reflect, Debug, PartialEq)]
struct Pair<A, B> {
    first: A,
    second: B,
}

#[derive(Reflect, Debug, PartialEq)]
struct Bounded<T: Clone>
where
    T: Default,
{
    value: T,
}

#[derive(Reflect, Debug, PartialEq)]
struct Buf<const N: usize> {
    data: [u8; N],
}

#[test]
fn generic_types() {
    let pair = Pair {
        first: 1_u32,
        second: "two".to_string(),
    };
    let info = Pair::<u32, String>::static_type_info();
    assert_eq!(info.path(), "derive::Pair<u32,String>");
    assert!(std::ptr::eq(info, Pair::<u32, String>::static_type_info()));
    assert!(!std::ptr::eq(info, Pair::<u8, u8>::static_type_info()));
    assert!(std::ptr::eq(Reflect::type_info(&pair), info));
    let value: &dyn Reflect = &pair;
    let ReflectRef::Struct(view) = value.reflect_ref() else {
        panic!("a struct is expected");
    };
    assert_eq!(view.field("first").unwrap().downcast_ref::<u32>(), Some(&1));
    assert_eq!(
        info.as_struct()
            .unwrap()
            .field("second")
            .unwrap()
            .ty()
            .info()
            .path(),
        "String"
    );
    assert_eq!(
        Vec::<Pair<u8, u8>>::static_type_info().path(),
        "Vec<derive::Pair<u8,u8>>"
    );
    assert_eq!(
        Bounded::<u8>::static_type_info().path(),
        "derive::Bounded<u8>"
    );
    assert_eq!(Buf::<4>::static_type_info().path(), "derive::Buf<4>");
    assert_eq!(
        Buf::<4>::static_type_info()
            .as_struct()
            .unwrap()
            .field("data")
            .unwrap()
            .ty()
            .info()
            .path(),
        "[u8;4]"
    );
}

#[derive(Reflect, Debug, PartialEq)]
#[reflect(name = "game::Hero", alias = "game::OldHero", alias = "game::Legacy")]
struct Hero {
    level: u8,
}

mod inner {
    use reflection::Reflect;

    #[derive(Reflect)]
    pub struct Deep {
        pub x: u8,
    }
}

#[derive(Reflect)]
struct Event {
    r#type: u8,
}

#[test]
fn type_paths() {
    let info = Hero::static_type_info();
    assert_eq!(info.path(), "game::Hero");
    assert_eq!(info.aliases(), &["game::OldHero", "game::Legacy"]);
    assert_eq!(info.key(), TypeKey::new("game::Hero"));
    assert_eq!(info.id(), TypeKey::new("game::Hero").id());
    assert_eq!(
        inner::Deep::static_type_info().path(),
        "derive::inner::Deep"
    );
    let event = Event { r#type: 3 };
    let value: &dyn Reflect = &event;
    let ReflectRef::Struct(view) = value.reflect_ref() else {
        panic!("a struct is expected");
    };
    assert_eq!(view.name_at(0), Some("type"));
    assert_eq!(view.field("type").unwrap().downcast_ref::<u8>(), Some(&3));
    let _ = inner::Deep { x: 0 }.x;
}

#[derive(Reflect, Debug, PartialEq)]
struct Node {
    value: u32,
    next: Option<Box<Node>>,
}

#[test]
fn recursive_derived_types() {
    let node = Node::static_type_info();
    let next = node.as_struct().unwrap().field("next").unwrap().ty().info();
    assert_eq!(next.path(), "Option<Box<derive::Node>>");
    let boxed = next
        .as_enum()
        .unwrap()
        .variant("Some")
        .unwrap()
        .field_at(0)
        .unwrap()
        .ty()
        .info();
    let inner = boxed
        .as_tuple_struct()
        .unwrap()
        .field_at(0)
        .unwrap()
        .ty()
        .info();
    assert!(std::ptr::eq(inner, node));

    let list = Node {
        value: 1,
        next: Some(Box::new(Node {
            value: 2,
            next: None,
        })),
    };
    let mut values = Vec::new();
    let mut current: &dyn Reflect = &list;
    loop {
        let ReflectRef::Struct(node) = current.reflect_ref() else {
            panic!("a struct is expected");
        };
        values.push(*node.field("value").unwrap().downcast_ref::<u32>().unwrap());
        let ReflectRef::Enum(next) = node.field("next").unwrap().reflect_ref() else {
            panic!("an enum is expected");
        };
        let Some(boxed) = next.field_at(0) else {
            break;
        };
        let ReflectRef::TupleStruct(boxed) = boxed.reflect_ref() else {
            panic!("a tuple struct is expected");
        };
        current = boxed.field(0).unwrap();
    }
    assert_eq!(values, vec![1, 2]);
}

#[test]
fn derived_types_in_containers() {
    let mut players: HashMap<String, Player> = HashMap::new();
    {
        let value: &mut dyn Reflect = &mut players;
        let ReflectMut::Map(view) = value.reflect_mut() else {
            panic!("a map is expected");
        };
        view.insert(Box::new("ada".to_string()), Box::new(player()))
            .unwrap();
        let key = "ada".to_string();
        let entry = view.get(&key).unwrap();
        assert_eq!(entry.downcast_ref::<Player>(), Some(&player()));
    }
    assert_eq!(players.len(), 1);
    assert_eq!(
        HashMap::<String, Player>::static_type_info().path(),
        "HashMap<String,derive::Player>"
    );
    let mut shapes = vec![Shape::Empty];
    let value: &mut dyn Reflect = &mut shapes;
    let ReflectMut::List(view) = value.reflect_mut() else {
        panic!("a list is expected");
    };
    view.push(Box::new(Shape::Circle(1.0))).unwrap();
    assert_eq!(view.len(), 2);
    assert_eq!(shapes[1], Shape::Circle(1.0));
}

fn assert_send_sync<T: Send + Sync>() {}

#[test]
fn derived_types_are_send_and_sync() {
    assert_send_sync::<Player>();
    assert_send_sync::<Shape>();
    assert_send_sync::<Pair<u8, String>>();
}
