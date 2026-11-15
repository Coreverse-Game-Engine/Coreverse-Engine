//! `Pool` through the public API: the way an asset or resource registry would use it.

use memory::{Handle, Pool};

struct Texture {
    name: String,
}

struct Mesh;

fn texture(name: &str) -> Texture {
    Texture {
        name: name.to_string(),
    }
}

#[test]
fn handles_are_typed_per_pool() {
    let mut textures: Pool<Texture> = Pool::new();
    let mut meshes: Pool<Mesh> = Pool::new();

    let grass: Handle<Texture> = textures.insert(texture("grass")).unwrap();
    let cube: Handle<Mesh> = meshes.insert(Mesh).unwrap();

    assert_eq!(textures.get(grass).map(|t| t.name.as_str()), Some("grass"));
    assert!(meshes.contains(cube));
    // `textures.get(cube)` does not compile: a `Handle<Mesh>` is not a `Handle<Texture>`.
}

#[test]
fn a_registry_never_resolves_a_stale_handle() {
    let mut pool: Pool<u32> = Pool::new();
    let mut live = Vec::new();
    let mut stale = Vec::new();

    // Many rounds of inserting and removing, reusing slots over and over.
    for round in 0..200u32 {
        for offset in 0..5 {
            let value = round * 10 + offset;
            let handle = pool.insert(value).unwrap();
            live.push((handle, value));
        }
        // Remove every other live value.
        let mut remove_next = true;
        let mut keep = Vec::new();
        for (handle, value) in live.drain(..) {
            if remove_next {
                assert_eq!(pool.remove(handle), Some(value));
                stale.push(handle);
            } else {
                keep.push((handle, value));
            }
            remove_next = !remove_next;
        }
        live = keep;
    }

    for (handle, value) in &live {
        assert_eq!(pool.get(*handle), Some(value));
    }
    for handle in &stale {
        assert!(!pool.contains(*handle), "{handle}");
        assert_eq!(pool.get(*handle), None);
    }
    assert_eq!(pool.len(), live.len());
    assert_eq!(pool.retired_slots(), 0);
}

#[test]
fn a_handle_survives_a_copy_and_a_round_trip_through_its_parts() {
    let mut pool = Pool::new();
    let handle = pool.insert("value").unwrap();
    let copy = handle;
    let rebuilt: Handle<&str> = Handle::new(handle.index(), handle.generation());
    assert_eq!(pool.get(copy), Some(&"value"));
    assert_eq!(pool.get(rebuilt), Some(&"value"));
    assert_eq!(handle, rebuilt);
}
