//! R4 tests: `clone_value`, `reflect_eq`, dynamic values and `from_reflect`.

use std::collections::{BTreeMap, BTreeSet};

use reflection::{
    DynamicArray, DynamicEnum, DynamicList, DynamicMap, DynamicSet, DynamicStruct, DynamicTuple,
    DynamicTupleStruct, DynamicVariant, List, Map, OpaqueInfo, Reflect, ReflectError, ReflectKind,
    ReflectMut, ReflectRef, Set, TypeInfo, TypeInfoCell, TypeKey, TypeRegistry, Typed,
    impl_reflect_boilerplate,
};

#[derive(Reflect, Debug, PartialEq, Clone)]
enum Mode {
    Fast,
    #[reflect(alias = "Slow")]
    Careful {
        depth: u8,
    },
    Mixed(u8, String),
}

#[derive(Reflect, Debug, PartialEq, Clone)]
struct Config {
    name: String,
    ratio: f32,
    tags: Vec<String>,
    map: BTreeMap<String, u32>,
    set: BTreeSet<u8>,
    pair: (u8, u8),
    arr: [u8; 2],
    mode: Mode,
    next: Option<Box<Config>>,
}

fn leaf() -> Config {
    Config {
        name: "leaf".to_string(),
        ratio: 1.5,
        tags: Vec::new(),
        map: BTreeMap::new(),
        set: BTreeSet::new(),
        pair: (0, 0),
        arr: [0, 0],
        mode: Mode::Fast,
        next: None,
    }
}

fn config() -> Config {
    Config {
        name: "main".to_string(),
        ratio: 0.5,
        tags: vec!["a".to_string(), "b".to_string()],
        map: BTreeMap::from([("x".to_string(), 1), ("y".to_string(), 2)]),
        set: BTreeSet::from([3, 1]),
        pair: (1, 2),
        arr: [4, 5],
        mode: Mode::Careful { depth: 3 },
        next: Some(Box::new(leaf())),
    }
}

fn changed(change: impl FnOnce(&mut Config)) -> Config {
    let mut value = config();
    change(&mut value);
    value
}

#[test]
fn clone_value_makes_dynamic_values() {
    let config = config();
    let clone = config.clone_value().unwrap();
    assert_eq!(clone.kind(), ReflectKind::Struct);
    assert!(clone.is::<DynamicStruct>());
    assert_eq!(
        clone.represented_type_info().unwrap().path(),
        "clone_eq::Config"
    );
    assert_eq!(clone.type_info().path(), "DynamicStruct");
    assert_eq!(clone.type_info().kind(), ReflectKind::Opaque);

    let ReflectRef::Struct(view) = clone.reflect_ref() else {
        panic!("a struct is expected");
    };
    assert_eq!(view.field_len(), 9);
    assert!(view.field("name").unwrap().is::<String>());
    assert!(view.field("ratio").unwrap().is::<f32>());
    assert!(view.field("tags").unwrap().is::<DynamicList>());
    assert!(view.field("map").unwrap().is::<DynamicMap>());
    assert!(view.field("set").unwrap().is::<DynamicSet>());
    assert!(view.field("pair").unwrap().is::<DynamicTuple>());
    assert!(view.field("arr").unwrap().is::<DynamicArray>());
    assert!(view.field("mode").unwrap().is::<DynamicEnum>());
    assert!(view.field("next").unwrap().is::<DynamicEnum>());

    let ReflectRef::Enum(mode) = view.field("mode").unwrap().reflect_ref() else {
        panic!("an enum is expected");
    };
    assert_eq!(mode.variant_name(), "Careful");
    assert_eq!(mode.variant_index(), 1);
    assert_eq!(mode.field("depth").unwrap().downcast_ref::<u8>(), Some(&3));
    let represented = view.field("mode").unwrap().represented_type_info().unwrap();
    assert_eq!(represented.path(), "clone_eq::Mode");

    // A box inside is a tuple struct with one field.
    let ReflectRef::Enum(next) = view.field("next").unwrap().reflect_ref() else {
        panic!("an enum is expected");
    };
    assert!(next.field_at(0).unwrap().is::<DynamicTupleStruct>());
}

#[test]
fn reflect_eq_compares_structure() {
    let config = config();
    let clone = config.clone_value().unwrap();
    assert_eq!(config.reflect_eq(&*clone), Some(true));
    assert_eq!(clone.reflect_eq(&config), Some(true));
    assert_eq!(config.reflect_eq(&config.clone()), Some(true));

    let others = [
        changed(|c| c.name.push('!')),
        changed(|c| c.ratio = 9.0),
        changed(|c| c.tags.push("z".to_string())),
        changed(|c| {
            c.map.insert("x".to_string(), 9);
        }),
        changed(|c| {
            c.map.insert("w".to_string(), 1);
            c.map.remove("y");
        }),
        changed(|c| {
            c.set.insert(7);
        }),
        changed(|c| c.pair.0 = 9),
        changed(|c| c.arr[1] = 0),
        changed(|c| c.mode = Mode::Mixed(1, "m".to_string())),
        changed(|c| c.mode = Mode::Careful { depth: 4 }),
        changed(|c| c.next = None),
    ];
    for other in &others {
        assert_eq!(config.reflect_eq(other), Some(false));
        assert_eq!(clone.reflect_eq(other), Some(false));
        assert_eq!(other.reflect_eq(&*clone), Some(false));
    }
}

#[derive(Reflect)]
struct A {
    x: u32,
}

#[derive(Reflect)]
struct B {
    x: u32,
}

#[derive(Reflect)]
struct Wobbly {
    v: f32,
}

#[test]
fn equality_ignores_the_type_and_follows_the_leaves() {
    assert_eq!(A { x: 1 }.reflect_eq(&B { x: 1 }), Some(true));
    assert_eq!(A { x: 1 }.reflect_eq(&B { x: 2 }), Some(false));
    assert_eq!(1_u32.reflect_eq(&1_u32), Some(true));
    assert_eq!(1_u32.reflect_eq(&1_u64), Some(false));
    assert_eq!(1_u32.reflect_eq(&"a".to_string()), Some(false));
    assert_eq!(A { x: 1 }.reflect_eq(&vec![1_u32]), Some(false));
    // NaN is not equal to itself, as with `==`.
    assert_eq!(
        Wobbly { v: f32::NAN }.reflect_eq(&Wobbly { v: f32::NAN }),
        Some(false)
    );
}

#[derive(Debug, PartialEq)]
struct Blob(u8);

impl Typed for Blob {
    fn static_type_info() -> &'static TypeInfo {
        static CELL: TypeInfoCell = TypeInfoCell::new();
        CELL.get_or_init(|| {
            TypeInfo::Opaque(OpaqueInfo::new::<Blob>(TypeKey::new("clone_eq::Blob")))
        })
    }
}

impl Reflect for Blob {
    impl_reflect_boilerplate!();

    fn reflect_ref(&self) -> ReflectRef<'_> {
        ReflectRef::Opaque(self)
    }

    fn reflect_mut(&mut self) -> ReflectMut<'_> {
        ReflectMut::Opaque(self)
    }
}

#[test]
fn opaque_values_without_clone_and_eq_are_reported() {
    assert_eq!(Blob(1).reflect_eq(&Blob(1)), None);
    assert_eq!(vec![Blob(1)].reflect_eq(&vec![Blob(1)]), None);
    assert_eq!(
        vec![Blob(1)].reflect_eq(&vec![Blob(1), Blob(2)]),
        Some(false)
    );
    assert_eq!(
        Blob(1).clone_value().err(),
        Some(ReflectError::unsupported("clone", "clone_eq::Blob"))
    );
    assert_eq!(
        Blob(1).apply(&Blob(2)).err(),
        Some(ReflectError::unsupported("clone", "clone_eq::Blob"))
    );
    assert!(Blob::from_reflect(&Blob(1)).is_none());
    assert!(vec![Blob(1)].clone_value().is_err());
}

#[test]
fn from_reflect_builds_concrete_values_back() {
    let config = config();
    let clone = config.clone_value().unwrap();
    assert_eq!(Config::from_reflect(&*clone), Some(config.clone()));
    assert_eq!(Config::from_reflect(&config), Some(config.clone()));
    assert_eq!(Mode::from_reflect(&Mode::Fast), Some(Mode::Fast));
    assert_eq!(
        Option::<Box<Config>>::from_reflect(&Some(Box::new(leaf()))),
        Some(Some(Box::new(leaf())))
    );

    assert!(Config::from_reflect(&5_u32).is_none());
    assert!(Vec::<u8>::from_reflect(&5_u8).is_none());
    assert_eq!(<[u8; 2]>::from_reflect(&vec![1_u8, 2]), Some([1, 2]));
    assert!(<[u8; 2]>::from_reflect(&vec![1_u8]).is_none());
    assert_eq!(<(u8, u8)>::from_reflect(&(1_u8, 2_u8)), Some((1, 2)));
    assert!(<(u8, u8)>::from_reflect(&(1_u8, 2_u8, 3_u8)).is_none());
    assert_eq!(u32::from_reflect(&7_u32), Some(7));
    assert!(u32::from_reflect(&7_u64).is_none());
}

#[derive(Reflect, Debug, PartialEq)]
struct Extra {
    id: u32,
    #[reflect(default)]
    level: u8,
    #[reflect(skip)]
    cache: Vec<u8>,
    #[reflect(rename = "label", alias = "title")]
    text: String,
}

#[derive(Reflect)]
#[reflect(no_from_reflect)]
#[allow(dead_code)]
struct Locked {
    id: u32,
    #[reflect(skip)]
    token: Token,
}

struct Token;

#[test]
fn defaults_skipped_fields_and_aliases_in_from_reflect() {
    let mut patch = DynamicStruct::new();
    patch.insert("id", 5_u32);
    patch.insert("title", "hi".to_string());
    assert_eq!(
        Extra::from_reflect(&patch),
        Some(Extra {
            id: 5,
            level: 0,
            cache: Vec::new(),
            text: "hi".to_string()
        })
    );
    patch.insert("level", 3_u8);
    patch.insert("label", "yo".to_string());
    assert_eq!(
        Extra::from_reflect(&patch),
        Some(Extra {
            id: 5,
            level: 3,
            cache: Vec::new(),
            text: "yo".to_string()
        })
    );
    // A field that is neither `default` nor skipped is required.
    let mut missing = DynamicStruct::new();
    missing.insert("id", 1_u32);
    assert!(Extra::from_reflect(&missing).is_none());
    // A field of the wrong type is an error, `default` or not.
    let mut wrong = DynamicStruct::new();
    wrong.insert("id", 1_u32);
    wrong.insert("label", "x".to_string());
    wrong.insert("level", "high".to_string());
    assert!(Extra::from_reflect(&wrong).is_none());

    assert!(
        Locked::from_reflect(&Locked {
            id: 1,
            token: Token
        })
        .is_none()
    );
}

#[test]
fn enums_are_built_by_variant_name_or_alias() {
    let slow = DynamicEnum::new(
        "Slow",
        DynamicVariant::Struct(vec![("depth", Box::new(3_u8) as Box<dyn Reflect>)]),
    );
    assert_eq!(Mode::from_reflect(&slow), Some(Mode::Careful { depth: 3 }));
    let mixed = DynamicEnum::new(
        "Mixed",
        DynamicVariant::Tuple(vec![
            Box::new(1_u8) as Box<dyn Reflect>,
            Box::new("m".to_string()) as Box<dyn Reflect>,
        ]),
    );
    assert_eq!(
        Mode::from_reflect(&mixed),
        Some(Mode::Mixed(1, "m".to_string()))
    );
    assert_eq!(
        Mode::from_reflect(&DynamicEnum::unit("Fast")),
        Some(Mode::Fast)
    );
    assert!(Mode::from_reflect(&DynamicEnum::unit("Nope")).is_none());
    assert!(Mode::from_reflect(&DynamicEnum::unit("Mixed")).is_none());

    let some = DynamicEnum::new(
        "Some",
        DynamicVariant::Tuple(vec![Box::new(5_u32) as Box<dyn Reflect>]),
    );
    assert_eq!(Option::<u32>::from_reflect(&some), Some(Some(5)));
    assert_eq!(
        Option::<u32>::from_reflect(&DynamicEnum::unit("None")),
        Some(None)
    );
}

#[test]
fn the_registry_builds_values_by_name() {
    let mut registry = TypeRegistry::new();
    registry.register::<Config>().unwrap();
    registry.register::<Locked>().unwrap();
    let config = config();
    let clone = config.clone_value().unwrap();
    let built = registry.from_reflect("clone_eq::Config", &*clone).unwrap();
    assert_eq!(built.downcast_ref::<Config>(), Some(&config));
    assert_eq!(
        registry.from_reflect("clone_eq::Locked", &*clone).err(),
        Some(ReflectError::unsupported(
            "build from a reflected value",
            "clone_eq::Locked"
        ))
    );
    assert_eq!(
        registry.from_reflect("clone_eq::Config", &5_u32).err(),
        Some(ReflectError::unsupported(
            "build from a reflected value",
            "clone_eq::Config"
        ))
    );
    assert_eq!(
        registry.from_reflect("nope", &5_u32).err(),
        Some(ReflectError::UnknownType {
            name: "nope".to_string()
        })
    );
}

#[test]
fn dynamic_values_behave_like_their_kind() {
    assert_eq!(DynamicStruct::static_type_info().path(), "DynamicStruct");
    assert_eq!(DynamicList::new().kind(), ReflectKind::List);
    assert_eq!(DynamicMap::new().kind(), ReflectKind::Map);
    assert_eq!(DynamicSet::new().kind(), ReflectKind::Set);
    assert_eq!(DynamicEnum::unit("A").kind(), ReflectKind::Enum);
    assert!(DynamicStruct::new().represented_type_info().is_none());
    assert!(5_u32.represented_type_info().is_some());

    let mut list = DynamicList::new();
    list.push(Box::new(1_u32)).unwrap();
    list.push(Box::new(3_u32)).unwrap();
    list.insert(1, Box::new(2_u32)).unwrap();
    assert_eq!(
        list.insert(9, Box::new(0_u32)).err(),
        Some(ReflectError::index_out_of_bounds(9, 3))
    );
    assert_eq!(list.len(), 3);
    assert_eq!(list.get(1).unwrap().downcast_ref::<u32>(), Some(&2));
    assert_eq!(list.remove(0).unwrap().take::<u32>(), Ok(1));
    assert_eq!(list.pop().unwrap().take::<u32>(), Ok(3));
    assert!(list.remove(5).is_none());
    list.clear();
    assert!(list.is_empty());

    let mut map = DynamicMap::new();
    assert!(
        map.insert(Box::new("a".to_string()), Box::new(1_u32))
            .unwrap()
            .is_none()
    );
    let old = map
        .insert(Box::new("a".to_string()), Box::new(2_u32))
        .unwrap()
        .unwrap();
    assert_eq!(old.take::<u32>(), Ok(1));
    assert_eq!(map.len(), 1);
    assert_eq!(
        map.get(&"a".to_string()).unwrap().downcast_ref::<u32>(),
        Some(&2)
    );
    assert!(map.get(&"b".to_string()).is_none());
    assert_eq!(map.iter().count(), 1);
    assert!(map.remove(&"a".to_string()).is_some());
    assert!(map.is_empty());

    let mut set = DynamicSet::new();
    assert_eq!(set.insert(Box::new(1_u8)), Ok(true));
    assert_eq!(set.insert(Box::new(1_u8)), Ok(false));
    assert!(set.contains(&1_u8));
    assert!(!set.contains(&2_u8));
    assert!(set.remove(&1_u8));
    assert!(!set.remove(&1_u8));
    assert!(set.is_empty());

    let mut tuple_struct = DynamicTupleStruct::new();
    tuple_struct.push(1_u8);
    assert_eq!(tuple_struct.len(), 1);
    let mut tuple = DynamicTuple::new();
    tuple.push(1_u8);
    tuple.push_boxed(Box::new(2_u8));
    assert_eq!(tuple.len(), 2);
    assert!(!tuple.is_empty());
}
