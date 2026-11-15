# Memory

`engine/rust/crates/memory` is the engine's memory layer. It is being built in milestones; this page describes what
exists **today** (M1, the foundation, and M2, arenas). Pools and allocation tracking are planned and not implemented
yet.

## What is there

### Foundation (M1)

| Item                              | Purpose                                                                                                                      |
|:----------------------------------|:-----------------------------------------------------------------------------------------------------------------------------|
| `Alignment`                       | A validated power-of-two alignment: `new`, `of::<T>()`, `of_layout`, `align_up`, `align_down`, `is_aligned`, `padding_for`.  |
| `layout_from_parts(size, align)`  | A `Layout` from raw numbers, with every check reported as an error.                                                          |
| `array_layout(element, count)`    | The layout of `count` elements placed like a Rust array (stride = size rounded up to the alignment).                         |
| `ByteSize`                        | A byte count for budgets and reports: binary units, checked/saturating arithmetic, `Display` (`1.5 KiB`, `3.0 MiB`, ...).    |
| `MemoryError`, `MemoryResult`     | `InvalidAlignment`, `SizeOverflow`, `OutOfMemory`, `AllocationFailed`, `InvalidMarker`. `#[non_exhaustive]`.                 |

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

## Conventions

* **No panics on bad input.** Sizes and alignments that come from outside are validated and reported as a
  `MemoryError`. `ByteSize` offers checked and saturating arithmetic only, no operators.
* **`unsafe` in one private module.** The workspace denies `unsafe_code`; `bump` is allowed it, with the reason spelled
  out in the `#[allow]`. Every `unsafe` block carries a safety comment, the invariants are listed at the top of the
  module, and the public API is safe. `FrameArena` is plain safe code on top of `Bump`.
* **No dependencies, stable Rust.** `allocator_api` is unstable, so allocators cannot be plugged into `Vec<T, A>`; the
  arenas hand out references and slices instead of depending on an allocator crate.

## Roadmap

| Milestone | Content                                                                                   | State   |
|:----------|:------------------------------------------------------------------------------------------|:--------|
| M1        | `Alignment`, checked layouts, `ByteSize`, `MemoryError`                                   | done    |
| M2        | `Bump` (with markers), `FrameArena`; the first `unsafe`                                   | done    |
| M3        | `Pool<T>` with `ccore::Handle<T>`: stale handles are rejected                             | planned |
| M4        | `MemoryTag`, atomic counters, opt-in `TrackingAllocator`, snapshots, budgets, leak report | planned |
| M5        | FFI: `ffi_memory_stats` and a `cv-ffi` wrapper; optional C++ allocation hooks             | planned |
| M6        | criterion benchmarks, remaining docs                                                      | planned |

There is no separate `StackArena`: a `Bump` with markers is the same thing. Out of scope for now: allocators for the
ECS (archetype chunks) and for the job system (thread-local scratch space); they will be designed together with `ecs`
and `threading`.
