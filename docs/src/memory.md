# Memory

`engine/rust/crates/memory` is the engine's memory layer. It is being built in milestones; this page describes what
exists **today** (M1, the foundation, M2, arenas, and M3, the pool). Allocation tracking is planned and not implemented
yet.

## What is there

### Foundation (M1)

| Item                              | Purpose                                                                                                                      |
|:----------------------------------|:-----------------------------------------------------------------------------------------------------------------------------|
| `Alignment`                       | A validated power-of-two alignment: `new`, `of::<T>()`, `of_layout`, `align_up`, `align_down`, `is_aligned`, `padding_for`.  |
| `layout_from_parts(size, align)`  | A `Layout` from raw numbers, with every check reported as an error.                                                          |
| `array_layout(element, count)`    | The layout of `count` elements placed like a Rust array (stride = size rounded up to the alignment).                         |
| `ByteSize`                        | A byte count for budgets and reports: binary units, checked/saturating arithmetic, `Display` (`1.5 KiB`, `3.0 MiB`, ...).    |
| `MemoryError`, `MemoryResult`     | `InvalidAlignment`, `SizeOverflow`, `OutOfMemory`, `AllocationFailed`, `PoolFull`, `InvalidMarker`. `#[non_exhaustive]`.   |

### Arenas (M2)

| Item         | Purpose                                                                                                                                      |
|:-------------|:---------------------------------------------------------------------------------------------------------------------------------------------|
| `Bump`       | A fixed-capacity bump allocator for `Copy` values: `alloc`, `alloc_slice_copy`, `alloc_slice_fill`, `alloc_slice_fill_with`, counters (`capacity`, `used`, `remaining`, `peak`). |
| `Marker`     | A position in a `Bump` (`marker()`); `rewind(marker)` frees everything allocated after it (stack-style scratch space).                       |
| `FrameArena` | A `Bump` that is emptied once per frame with `begin_frame()`, plus the frame number and the previous frame's usage. Dereferences to `Bump`.   |

```rust,ignore
use memory::{Bump, FrameArena};

// Allocation takes `&self` and returns a reference; freeing takes `&mut self`.
let mut scratch = Bump::with_capacity(4096)?;
let name = scratch.alloc_slice_copy(b"player")?;
let health = scratch.alloc(100u32)?;
*health -= 25;
scratch.reset(); // `name` and `health` cannot be used after this line: the compiler rejects it

// One arena per frame.
let mut frames = FrameArena::with_capacity(1 << 20)?;
loop {
    let draw_list = frames.alloc_slice_fill(256, 0u32)?;
    // ... build and consume the draw list ...
    frames.begin_frame();
}
```

How it works: every allocation method borrows the arena (`&self`) and returns a reference into its block, so many values
can be alive at once and nothing is freed one by one. `reset`, `rewind` and `begin_frame` need `&mut self`; the borrow
checker therefore refuses them while any reference into the arena is alive, and there is no way to read freed memory.
Only `Copy` values are accepted: they have no destructor, so reusing the memory without running destructors loses
nothing.

Limits, on purpose:

* **The capacity is fixed.** A request that does not fit fails with `MemoryError::OutOfMemory { requested, available }`
  and changes nothing. Use `peak()` to find the right size.
* **No double-buffered frame arena.** Reading frame N's data during frame N + 1 would need a reference to cross
  `begin_frame`, which Rust does not allow. If a system needs data to outlive a frame it needs another design (handles
  or offsets), which is not built until a system asks for it.
* **A `Marker` only helps when everything allocated before it is dead too.** `rewind` needs `&mut self`, so the
  compiler requires every reference into the arena to be dead, not only those allocated after the marker. This makes the
  marker useful for scratch work between two phases, not for nested scopes that keep outer data alive.
* **A `Bump` is `Send` but not `Sync`.** It can move to another thread, not be shared between threads.

### Pool (M3)

| Item   | Purpose                                                                                                                                                      |
|:-------|:-------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `Pool` | Stores values of any type `T` and addresses them by `Handle<T>` (re-exported from `ccore`): `insert`, `get`, `get_mut`, `contains`, `remove`, `clear`, `iter`, `iter_mut`, `len`, `capacity`, `retired_slots`. |

```rust,ignore
use memory::Pool;

let mut textures: Pool<String> = Pool::new();
let grass = textures.insert("grass.png".to_string())?;
textures.remove(grass);
let sand = textures.insert("sand.png".to_string())?; // reuses the slot of `grass` ...
assert!(textures.get(grass).is_none());              // ... but `grass` is stale
assert!(textures.get(sand).is_some());
```

A handle is a slot index plus a generation. Removing a value bumps the slot's generation, so every older handle to it is
stale: `get`, `get_mut`, `contains` and `remove` answer `None` for it, also after the slot has been reused. A stale,
forged or never-issued handle can therefore not reach a wrong value and is never undefined behaviour. The pool is entirely
safe code (no `unsafe`).

* **Handles do not know their pool.** A handle used on another `Pool<T>` than the one that issued it may find an
  unrelated value. Keep handles with their pool.
* **Reuse and growth.** The slot that was freed last is reused first. The pool grows when no slot is free and never
  shrinks; `Pool::with_capacity(n)` reserves room so that `n` inserts do not allocate. Growth failure is an error
  (`AllocationFailed`), not an abort.
* **Retired slots.** After 2^32 - 1 reuses a slot's generation counter is exhausted; the slot is retired and never used
  again (`retired_slots()`), instead of letting very old handles look valid again. It costs one slot until the pool is
  dropped.
* **At most 2^32 slots** (`PoolFull`), because an index is a `u32`.
* **On an insert error the value is dropped.**
* **Dependency.** The pool needs `ccore::Handle`, so `memory` depends on `ccore` (which has no dependency on `memory`).

## Conventions

* **No panics on bad input.** Sizes and alignments that come from outside are validated and reported as a
  `MemoryError`. `ByteSize` offers checked and saturating arithmetic only, no operators.
* **`unsafe` in one private module.** The workspace denies `unsafe_code`; `bump` is allowed it, with the reason spelled
  out in the `#[allow]`. Every `unsafe` block carries a safety comment, the invariants are listed at the top of the
  module, and the public API is safe. `FrameArena` and `Pool` are plain safe code.
* **One dependency (`ccore`, for `Handle`), stable Rust.** `allocator_api` is unstable, so allocators cannot be plugged into `Vec<T, A>`; the
  arenas hand out references and slices instead of depending on an allocator crate.

## Roadmap

| Milestone | Content                                                                                   | State   |
|:----------|:------------------------------------------------------------------------------------------|:--------|
| M1        | `Alignment`, checked layouts, `ByteSize`, `MemoryError`                                   | done    |
| M2        | `Bump` (with markers), `FrameArena`; the first `unsafe`                                   | done    |
| M3        | `Pool<T>` with `ccore::Handle<T>`: stale handles are rejected                             | done    |
| M4        | `MemoryTag`, atomic counters, opt-in `TrackingAllocator`, snapshots, budgets, leak report | planned |
| M5        | FFI: `ffi_memory_stats` and a `cv-ffi` wrapper; optional C++ allocation hooks             | planned |
| M6        | criterion benchmarks, remaining docs                                                      | planned |

There is no separate `StackArena`: a `Bump` with markers is the same thing. Out of scope for now: allocators for the
ECS (archetype chunks) and for the job system (thread-local scratch space); they will be designed together with `ecs`
and `threading`.
