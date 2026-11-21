//! R1 tests: a hand-written reflected struct and enum, and the standard-library implementations.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use reflection::{
    FieldInfo, GenericTypeInfoCell, Reflect, ReflectError, ReflectKind, ReflectMut, ReflectRef,
    Struct, StructInfo, TypeInfo, TypeInfoCell, TypeKey, TypeRef, Typed, impl_reflect_boilerplate,
};

#[derive(Debug, PartialEq, Clone)]
struct Player {
    name: String,
    health: u32,
    position: (f32, f32),
    tags: Vec<String>,
    weapon: Option<String>,
}

impl Typed for Player {
    fn static_type_info() -> &'static TypeInfo {
        static CELL: TypeInfoCell = TypeInfoCell::new();
        CELL.get_or_init(|| {
            TypeInfo::Struct(StructInfo::new::<Player>(
                TypeKey::new("tests::Player"),
                vec![
                    FieldInfo::named("name", 0, TypeRef::of::<String>()),
                    FieldInfo::named("health", 1, TypeRef::of::<u32>()),
                    FieldInfo::named("position", 2, TypeRef::of::<(f32, f32)>()),
                    FieldInfo::named("tags", 3, TypeRef::of::<Vec<String>>()),
                    FieldInfo::named("weapon", 4, TypeRef::of::<Option<String>>()),
                ],
            ))
        })
    }
}

impl Reflect for Player {
    impl_reflect_boilerplate!();

    fn reflect_ref(&self) -> ReflectRef<'_> {
        ReflectRef::Struct(self)
    }

    fn reflect_mut(&mut self) -> ReflectMut<'_> {
        ReflectMut::Struct(self)
    }
}

impl Struct for Player {
    fn field_len(&self) -> usize {
        5
    }

    fn name_at(&self, index: usize) -> Option<&'static str> {
        Self::static_type_info()
            .as_struct()
            .and_then(|info| info.field_at(index))
            .and_then(FieldInfo::name)
    }

    fn field(&self, name: &str) -> Option<&dyn Reflect> {
        match name {
            "name" => Some(&self.name),
            "health" => Some(&self.health),
            "position" => Some(&self.position),
            "tags" => Some(&self.tags),
            "weapon" => Some(&self.weapon),
            _ => None,
        }
    }

    fn field_mut(&mut self, name: &str) -> Option<&mut dyn Reflect> {
        match name {
            "name" => Some(&mut self.name),
            "health" => Some(&mut self.health),
            "position" => Some(&mut self.position),
            "tags" => Some(&mut self.tags),
            "weapon" => Some(&mut self.weapon),
            _ => None,
        }
    }

    fn field_at(&self, index: usize) -> Option<&dyn Reflect> {
        let name = self.name_at(index)?;
        self.field(name)
    }

    fn field_at_mut(&mut self, index: usize) -> Option<&mut dyn Reflect> {
        let name = self.name_at(index)?;
        self.field_mut(name)
    }
}

fn player() -> Player {
    Player {
        name: "Ada".to_string(),
        health: 100,
        position: (1.0, 2.0),
        tags: vec!["mage".to_string()],
        weapon: None,
    }
}

#[test]
fn reads_fields_by_name_and_index() {
    let player = player();
    let value: &dyn Reflect = &player;
    let ReflectRef::Struct(view) = value.reflect_ref() else {
        panic!("a struct is expected");
    };
    assert_eq!(view.field_len(), 5);
    assert_eq!(view.name_at(1), Some("health"));
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
    assert!(view.field("missing").is_none());
    assert!(view.field_at(9).is_none());
}

#[test]
fn writes_fields_through_reflection() {
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
        view.field_mut("name")
            .unwrap()
            .set(Box::new("Grace".to_string()))
            .unwrap();
    }
    assert_eq!(player.health, 40);
    assert_eq!(player.name, "Grace");
}

#[test]
fn set_rejects_a_value_of_another_type() {
    let mut player = player();
    let value: &mut dyn Reflect = &mut player;
    let view = match value.reflect_mut() {
        ReflectMut::Struct(view) => view,
        _ => panic!("a struct is expected"),
    };
    let err = view
        .field_mut("health")
        .unwrap()
        .set(Box::new("full".to_string()))
        .unwrap_err();
    assert_eq!(err, ReflectError::type_mismatch("u32", "String"));
    assert_eq!(player.health, 100);
}

#[test]
fn whole_value_set_replaces_the_struct() {
    let mut target = player();
    let mut other = player();
    other.health = 7;
    target.set(Box::new(other.clone())).unwrap();
    assert_eq!(target, other);
}

#[test]
fn describes_the_struct() {
    let info = Player::static_type_info();
    assert_eq!(info.kind(), ReflectKind::Struct);
    assert_eq!(info.path(), "tests::Player");
    assert!(info.is::<Player>());
    let info = info.as_struct().unwrap();
    assert_eq!(info.field_len(), 5);
    assert_eq!(info.index_of("tags"), Some(3));
    assert_eq!(info.field("health").unwrap().ty().info().path(), "u32");
    assert_eq!(
        info.field("position").unwrap().ty().info().path(),
        "(f32,f32)"
    );
    assert_eq!(
        info.field("tags").unwrap().ty().info().path(),
        "Vec<String>"
    );
    assert_eq!(
        info.field("weapon").unwrap().ty().info().path(),
        "Option<String>"
    );
    assert!(info.field("health").unwrap().ty().is::<u32>());
}

#[test]
fn descriptions_are_shared_and_keys_are_stable() {
    assert!(std::ptr::eq(
        Vec::<u32>::static_type_info(),
        Vec::<u32>::static_type_info()
    ));
    assert!(!std::ptr::eq(
        Vec::<u32>::static_type_info(),
        Vec::<u64>::static_type_info()
    ));
    let value = vec![1_u32];
    assert!(std::ptr::eq(
        Reflect::type_info(&value),
        Vec::<u32>::static_type_info()
    ));
    assert_eq!(u32::static_type_info().key(), TypeKey::new("u32"));
    assert_ne!(u32::static_type_info().id(), u64::static_type_info().id());
}

#[test]
fn opaque_values_read_and_write() {
    let mut number = 5_i32;
    {
        let value: &mut dyn Reflect = &mut number;
        assert_eq!(value.kind(), ReflectKind::Opaque);
        assert!(value.is::<i32>());
        assert!(!value.is::<u32>());
        *value.downcast_mut::<i32>().unwrap() += 1;
        value.set(Box::new(10_i32)).unwrap();
    }
    assert_eq!(number, 10);
    let boxed: Box<dyn Reflect> = Box::new(3_u8);
    assert_eq!(boxed.take::<u8>(), Ok(3));
    let boxed: Box<dyn Reflect> = Box::new(3_u8);
    assert_eq!(
        boxed.take::<u16>(),
        Err(ReflectError::type_mismatch("u16", "u8"))
    );
}

#[test]
fn list_operations() {
    let mut numbers: Vec<u32> = vec![1, 2, 3];
    {
        let value: &mut dyn Reflect = &mut numbers;
        let ReflectMut::List(list) = value.reflect_mut() else {
            panic!("a list is expected");
        };
        assert_eq!(list.len(), 3);
        assert!(!list.is_empty());
        list.push(Box::new(4_u32)).unwrap();
        list.insert(0, Box::new(0_u32)).unwrap();
        assert_eq!(
            list.insert(99, Box::new(1_u32)),
            Err(ReflectError::index_out_of_bounds(99, 5))
        );
        assert_eq!(
            list.push(Box::new("x".to_string())),
            Err(ReflectError::type_mismatch("u32", "String"))
        );
        *list.get_mut(1).unwrap().downcast_mut::<u32>().unwrap() = 10;
        let removed = list.remove(2).unwrap();
        assert_eq!(removed.take::<u32>(), Ok(2));
        assert!(list.remove(50).is_none());
        let popped = list.pop().unwrap();
        assert_eq!(popped.take::<u32>(), Ok(4));
        let seen: Vec<u32> = list
            .iter()
            .map(|item| *item.downcast_ref::<u32>().unwrap())
            .collect();
        assert_eq!(seen, vec![0, 10, 3]);
        list.clear();
        assert!(list.is_empty());
    }
    assert!(numbers.is_empty());
}

#[test]
fn array_operations() {
    let mut array = [1_u8, 2, 3];
    let value: &mut dyn Reflect = &mut array;
    let ReflectMut::Array(view) = value.reflect_mut() else {
        panic!("an array is expected");
    };
    assert_eq!(view.len(), 3);
    *view.get_mut(2).unwrap().downcast_mut::<u8>().unwrap() = 9;
    assert!(view.get(3).is_none());
    assert_eq!(view.iter().count(), 3);
    assert_eq!(array, [1, 2, 9]);
    let info = <[u8; 3]>::static_type_info();
    assert_eq!(info.path(), "[u8;3]");
    assert_eq!(info.as_array().unwrap().len(), 3);
}

#[test]
fn tuple_operations() {
    let mut pair = (1_u32, "two".to_string());
    {
        let value: &mut dyn Reflect = &mut pair;
        let ReflectMut::Tuple(view) = value.reflect_mut() else {
            panic!("a tuple is expected");
        };
        assert_eq!(view.field_len(), 2);
        *view.field_mut(0).unwrap().downcast_mut::<u32>().unwrap() = 5;
        assert!(view.field(2).is_none());
    }
    assert_eq!(pair.0, 5);
    let info = <(u32, String)>::static_type_info();
    assert_eq!(info.path(), "(u32,String)");
    assert_eq!(info.as_tuple().unwrap().field_len(), 2);
    assert_eq!(<(u8,)>::static_type_info().path(), "(u8)");
}

#[test]
fn option_is_an_enum() {
    let mut value: Option<u32> = None;
    {
        let view: &dyn Reflect = &value;
        let ReflectRef::Enum(view) = view.reflect_ref() else {
            panic!("an enum is expected");
        };
        assert_eq!(view.variant_name(), "None");
        assert_eq!(view.variant_index(), 0);
        assert_eq!(view.field_len(), 0);
        assert!(view.field_at(0).is_none());
    }
    value.set(Box::new(Some(7_u32))).unwrap();
    {
        let view: &mut dyn Reflect = &mut value;
        let ReflectMut::Enum(view) = view.reflect_mut() else {
            panic!("an enum is expected");
        };
        assert_eq!(view.variant_name(), "Some");
        assert_eq!(view.field_len(), 1);
        *view.field_at_mut(0).unwrap().downcast_mut::<u32>().unwrap() = 8;
        assert!(view.field_at(1).is_none());
    }
    assert_eq!(value, Some(8));
    let info = Option::<u32>::static_type_info().as_enum().unwrap();
    assert_eq!(info.variant_len(), 2);
    assert_eq!(info.variant("Some").unwrap().field_len(), 1);
    assert_eq!(info.variant_at(0).unwrap().name(), "None");
}

#[test]
fn hash_map_operations() {
    let mut map: HashMap<String, u32> = HashMap::new();
    let value: &mut dyn Reflect = &mut map;
    let ReflectMut::Map(view) = value.reflect_mut() else {
        panic!("a map is expected");
    };
    assert!(
        view.insert(Box::new("a".to_string()), Box::new(1_u32))
            .unwrap()
            .is_none()
    );
    let old = view
        .insert(Box::new("a".to_string()), Box::new(2_u32))
        .unwrap()
        .unwrap();
    assert_eq!(old.take::<u32>(), Ok(1));
    assert_eq!(
        view.insert(Box::new(5_u32), Box::new(1_u32)).err(),
        Some(ReflectError::type_mismatch("String", "u32"))
    );
    let key = "a".to_string();
    assert_eq!(view.get(&key).unwrap().downcast_ref::<u32>(), Some(&2));
    assert!(view.get(&5_u32).is_none());
    *view.get_mut(&key).unwrap().downcast_mut::<u32>().unwrap() = 3;
    assert_eq!(view.len(), 1);
    assert_eq!(view.iter().count(), 1);
    assert_eq!(view.remove(&key).unwrap().take::<u32>(), Ok(3));
    assert!(view.is_empty());
    view.clear();
}

#[test]
fn btree_map_iterates_in_order() {
    let mut map: BTreeMap<u32, String> = BTreeMap::new();
    let value: &mut dyn Reflect = &mut map;
    let ReflectMut::Map(view) = value.reflect_mut() else {
        panic!("a map is expected");
    };
    view.insert(Box::new(2_u32), Box::new("two".to_string()))
        .unwrap();
    view.insert(Box::new(1_u32), Box::new("one".to_string()))
        .unwrap();
    let keys: Vec<u32> = view
        .iter()
        .map(|(key, _)| *key.downcast_ref::<u32>().unwrap())
        .collect();
    assert_eq!(keys, vec![1, 2]);
    assert_eq!(
        BTreeMap::<u32, String>::static_type_info().path(),
        "BTreeMap<u32,String>"
    );
}

#[test]
fn set_operations() {
    let mut set: HashSet<u32> = HashSet::new();
    {
        let value: &mut dyn Reflect = &mut set;
        let ReflectMut::Set(view) = value.reflect_mut() else {
            panic!("a set is expected");
        };
        assert!(view.insert(Box::new(1_u32)).unwrap());
        assert!(!view.insert(Box::new(1_u32)).unwrap());
        assert_eq!(
            view.insert(Box::new("x".to_string())),
            Err(ReflectError::type_mismatch("u32", "String"))
        );
        assert!(view.contains(&1_u32));
        assert!(!view.contains(&"x".to_string()));
        assert_eq!(view.iter().count(), 1);
        assert!(view.remove(&1_u32));
        assert!(!view.remove(&1_u32));
        assert!(view.is_empty());
    }
    let mut ordered: BTreeSet<u8> = BTreeSet::new();
    let value: &mut dyn Reflect = &mut ordered;
    let ReflectMut::Set(view) = value.reflect_mut() else {
        panic!("a set is expected");
    };
    view.insert(Box::new(3_u8)).unwrap();
    view.insert(Box::new(1_u8)).unwrap();
    let items: Vec<u8> = view
        .iter()
        .map(|item| *item.downcast_ref::<u8>().unwrap())
        .collect();
    assert_eq!(items, vec![1, 3]);
}

#[test]
fn box_is_a_tuple_struct_with_one_field() {
    let mut boxed: Box<u32> = Box::new(4);
    {
        let value: &mut dyn Reflect = &mut boxed;
        let ReflectMut::TupleStruct(view) = value.reflect_mut() else {
            panic!("a tuple struct is expected");
        };
        assert_eq!(view.field_len(), 1);
        *view.field_mut(0).unwrap().downcast_mut::<u32>().unwrap() = 5;
        assert!(view.field(1).is_none());
    }
    assert_eq!(*boxed, 5);
    assert_eq!(Box::<u32>::static_type_info().path(), "Box<u32>");
}

#[derive(Debug, PartialEq)]
struct Node {
    value: u32,
    next: Option<Box<Node>>,
}

impl Typed for Node {
    fn static_type_info() -> &'static TypeInfo {
        static CELL: TypeInfoCell = TypeInfoCell::new();
        CELL.get_or_init(|| {
            TypeInfo::Struct(StructInfo::new::<Node>(
                TypeKey::new("tests::Node"),
                vec![
                    FieldInfo::named("value", 0, TypeRef::of::<u32>()),
                    FieldInfo::named("next", 1, TypeRef::of::<Option<Box<Node>>>()),
                ],
            ))
        })
    }
}

impl Reflect for Node {
    impl_reflect_boilerplate!();

    fn reflect_ref(&self) -> ReflectRef<'_> {
        ReflectRef::Struct(self)
    }

    fn reflect_mut(&mut self) -> ReflectMut<'_> {
        ReflectMut::Struct(self)
    }
}

impl Struct for Node {
    fn field_len(&self) -> usize {
        2
    }

    fn name_at(&self, index: usize) -> Option<&'static str> {
        ["value", "next"].get(index).copied()
    }

    fn field(&self, name: &str) -> Option<&dyn Reflect> {
        match name {
            "value" => Some(&self.value),
            "next" => Some(&self.next),
            _ => None,
        }
    }

    fn field_mut(&mut self, name: &str) -> Option<&mut dyn Reflect> {
        match name {
            "value" => Some(&mut self.value),
            "next" => Some(&mut self.next),
            _ => None,
        }
    }

    fn field_at(&self, index: usize) -> Option<&dyn Reflect> {
        let name = self.name_at(index)?;
        self.field(name)
    }

    fn field_at_mut(&mut self, index: usize) -> Option<&mut dyn Reflect> {
        let name = self.name_at(index)?;
        self.field_mut(name)
    }
}

#[test]
fn recursive_types_do_not_loop() {
    let node = Node::static_type_info();
    let next = node.as_struct().unwrap().field("next").unwrap().ty().info();
    assert_eq!(next.path(), "Option<Box<tests::Node>>");
    let some = next.as_enum().unwrap().variant("Some").unwrap();
    let boxed = some.field_at(0).unwrap().ty().info();
    assert_eq!(boxed.path(), "Box<tests::Node>");
    let inner = boxed
        .as_tuple_struct()
        .unwrap()
        .field_at(0)
        .unwrap()
        .ty()
        .info();
    assert!(std::ptr::eq(inner, node));
}

#[test]
fn walks_a_recursive_value() {
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
        let next = node.field("next").unwrap();
        let ReflectRef::Enum(next) = next.reflect_ref() else {
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
fn generic_cell_builds_once_per_instantiation_across_threads() {
    static CELL: GenericTypeInfoCell = GenericTypeInfoCell::new();
    let handles: Vec<_> = (0..8)
        .map(|_| {
            std::thread::spawn(|| {
                let info = CELL.get_or_insert::<u8>(|| {
                    TypeInfo::Opaque(reflection::OpaqueInfo::new::<u8>(TypeKey::new("cell::u8")))
                });
                info as *const TypeInfo as usize
            })
        })
        .collect();
    let addresses: Vec<usize> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert!(addresses.windows(2).all(|pair| pair[0] == pair[1]));
}

#[test]
fn errors_convert_into_core_errors() {
    let err: ccore::CoreError = ReflectError::index_out_of_bounds(2, 1).into();
    assert!(matches!(err, ccore::CoreError::InvalidArgument(_)));
}
