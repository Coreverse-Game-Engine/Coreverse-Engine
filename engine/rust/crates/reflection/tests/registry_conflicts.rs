//! R3 tests: two types that claim the same name. Every type here goes into the `inventory`, so
//! `TypeRegistry::from_inventory` fails in this test binary on purpose.

use reflection::{Reflect, ReflectError, TypeRegistry};

#[derive(Reflect)]
#[reflect(name = "dup::Thing")]
struct First;

#[derive(Reflect)]
#[reflect(name = "dup::Thing")]
struct Second;

#[derive(Reflect)]
struct Both {
    first: First,
    second: Second,
}

#[derive(Reflect)]
#[reflect(name = "dup::Own")]
struct Owner;

#[derive(Reflect)]
#[reflect(name = "dup::Other", alias = "dup::Own")]
struct Squatter;

#[test]
fn two_types_with_the_same_path_conflict() {
    let _ = (First, Second);
    let mut registry = TypeRegistry::new();
    registry.register::<First>().unwrap();
    assert_eq!(
        registry.register::<Second>().err(),
        Some(ReflectError::NameConflict {
            name: "dup::Thing",
            existing: "dup::Thing",
            new: "dup::Thing"
        })
    );
    assert_eq!(registry.len(), 1);
    registry.register::<First>().unwrap();
    assert_eq!(registry.len(), 1);
    assert!(
        registry
            .get_by_path("dup::Thing")
            .unwrap()
            .info()
            .is::<First>()
    );
}

#[test]
fn a_conflict_among_the_parts_registers_nothing() {
    let mut registry = TypeRegistry::new();
    let err = registry.register::<Both>().err();
    assert!(matches!(
        err,
        Some(ReflectError::NameConflict {
            name: "dup::Thing",
            ..
        })
    ));
    assert!(registry.is_empty());
    let both = Both {
        first: First,
        second: Second,
    };
    let _ = (&both.first, &both.second);
}

#[test]
fn an_alias_conflicts_with_a_path_in_either_order() {
    let _ = (Owner, Squatter);
    let mut registry = TypeRegistry::new();
    registry.register::<Owner>().unwrap();
    assert_eq!(
        registry.register::<Squatter>().err(),
        Some(ReflectError::NameConflict {
            name: "dup::Own",
            existing: "dup::Own",
            new: "dup::Other"
        })
    );

    let mut other = TypeRegistry::new();
    other.register::<Squatter>().unwrap();
    assert_eq!(
        other.register::<Owner>().err(),
        Some(ReflectError::NameConflict {
            name: "dup::Own",
            existing: "dup::Other",
            new: "dup::Own"
        })
    );
    assert!(
        other
            .get_by_path("dup::Own")
            .unwrap()
            .info()
            .is::<Squatter>()
    );
}

#[test]
fn from_inventory_reports_the_same_conflict_every_time() {
    // The roots are registered in the order of their paths: `dup::Other` (Squatter) before `dup::Own` (Owner).
    let expected = ReflectError::NameConflict {
        name: "dup::Own",
        existing: "dup::Other",
        new: "dup::Own",
    };
    assert_eq!(TypeRegistry::from_inventory().err(), Some(expected.clone()));
    assert_eq!(TypeRegistry::from_inventory().err(), Some(expected));
}
