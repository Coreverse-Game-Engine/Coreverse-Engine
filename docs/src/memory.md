# Memory

`engine/rust/crates/memory` is the engine's memory layer. It is being built in milestones; this page describes what
exists **today** (M1, the foundation, M2, arenas, M3, the pool, M4, tracking, and M5, the C++ side). Only the
benchmarks (M6) are still to do.

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

### Tracking (M4)

Memory is counted in two independent ways that answer different questions.

| Item                 | Purpose                                                                                                                                           |
|:---------------------|:--------------------------------------------------------------------------------------------------------------------------------------------------|
| `MemoryTag`          | The engine system a block belongs to: `General` (default), `Rendering`, `Assets`, `Ecs`, `Physics`, `Audio`, `Scripting`, `Ui`, `Networking`, `Tools`. |
| `MemoryTracker`      | Counts *registered* memory per tag (`record_alloc`, `record_dealloc`, `stats`, `total`), with an advisory budget per tag (`set_budget`, `is_over_budget`) and `report()`. |
| `global_tracker()`   | The process-wide `MemoryTracker`. `Bump::with_capacity_tagged` and `FrameArena::with_capacity_tagged` register their block in it for as long as they live. |
| `MemoryStats`        | A snapshot of one set of counters: `current`, `peak`, `total_allocated`, `allocations`, `deallocations`, `reallocations`, `live_allocations()`.    |
| `MemoryReport`       | The total plus one `TagReport` per used tag; `leaks()` lists the tags that still hold bytes, `over_budget()` those over their budget. Prints as plain text. |
| `TrackingAllocator`  | Wraps a `GlobalAlloc` (the system's by default) and counts every Rust allocation of the process. Nothing installs it; a program opts in with `#[global_allocator]`. |

```rust,ignore
use memory::{Bump, ByteSize, MemoryTag, global_tracker};

global_tracker().set_budget(MemoryTag::Rendering, Some(ByteSize::from_mib(64)));
let frame_memory = Bump::with_capacity_tagged(1 << 20, MemoryTag::Rendering)?;
// ... at shutdown:
let report = global_tracker().report();
for leak in report.leaks() {
    eprintln!("not freed: {leak}");
}

// A program that wants process-wide numbers:
use memory::TrackingAllocator;
use std::alloc::System;
#[global_allocator]
static ALLOCATOR: TrackingAllocator<System> = TrackingAllocator::new(System);
println!("{}", ALLOCATOR.stats());
```

What to know:

* **Tags apply to registered memory only.** The global allocator is not told which system is allocating, so
  `TrackingAllocator` has no per-tag numbers, and the tracker does not see memory nobody registered. Today the arenas
  (`Bump`, `FrameArena`) can register; `Pool` does not, and neither does anything that C++ code or the operating system
  allocates.
* **`TrackingAllocator` sees Rust allocations only,** and it is cheap but not free: a few relaxed atomic operations per
  allocation. It never allocates, locks or panics, which a global allocator must not do.
* **Budgets only warn.** Going over a budget changes `is_over_budget()` and the report, nothing else: no allocation is
  refused.
* **A leak is a tag with bytes still in use when you ask.** Call `report()` at shutdown (or at the end of a level);
  turning the report into log messages is the caller's job. The crate does not depend on `log-core`: emitting
  diagnostics needs a registered producer and diagnostic codes, which belongs to the integration step (M5).
* **Snapshots are approximately consistent** under load: the counters are read one after the other while other
  threads may be allocating.

### C++ side (M5)

The `ffi` crate exposes the global tracker to C++ (`ffi_memory_*`, see [Rust and C++ FFI](ffi.md)), and `cv-ffi` wraps
it in `cv_ffi::memory`:

* `RecordAlloc`/`RecordDealloc`, `TagStats`/`TotalStats`, budgets and `Report()`: the tracker's functions, with the same
  tags (`cv_ffi::memory::Tag`, mirrored by `static_assert`s).
* `TrackedAllocation`: a move-only RAII registration for a block C++ owns.
* `TrackingResource`: a `std::pmr::memory_resource` that registers every allocation under a tag and forwards to an
  upstream resource. This is the allocation hook for C++: hand it to a `std::pmr` container. `operator new` and
  `operator delete` are not replaced.
* `EmitReport(logger)`: the leak and budget report as log diagnostics (`CV-MEMORY-LEAK-<n>` and
  `CV-MEMORY-BUDGET-<n>`, warnings). The report is built in Rust and emitted through the Rust logger, so Rust and C++
  memory show up in one place.

The `TrackingAllocator` is not part of this interface: it is a global allocator, which the executable chooses.

## Conventions

* **No panics on bad input.** Sizes and alignments that come from outside are validated and reported as a
  `MemoryError`. `ByteSize` offers checked and saturating arithmetic only, no operators.
* **`unsafe` in two private modules.** The workspace denies `unsafe_code`; `bump` (the arena memory) and `tracking`
  (the `GlobalAlloc` implementation, which is an `unsafe trait`) are allowed it, with the reason spelled out in the
  `#[allow]`. Every `unsafe` block carries a safety comment, the invariants of `bump` are listed at the top of the
  module, and the public API is safe. `FrameArena`, `Pool`, `MemoryTracker` and the counters are plain safe code.
* **One dependency (`ccore`, for `Handle`), stable Rust.** `allocator_api` is unstable, so allocators cannot be plugged into `Vec<T, A>`; the
  arenas hand out references and slices instead of depending on an allocator crate.

## Roadmap

| Milestone | Content                                                                                   | State   |
|:----------|:------------------------------------------------------------------------------------------|:--------|
| M1        | `Alignment`, checked layouts, `ByteSize`, `MemoryError`                                   | done    |
| M2        | `Bump` (with markers), `FrameArena`; the first `unsafe`                                   | done    |
| M3        | `Pool<T>` with `ccore::Handle<T>`: stale handles are rejected                             | done    |
| M4        | `MemoryTag`, atomic counters, opt-in `TrackingAllocator`, snapshots, budgets, leak report | done    |
| M5        | FFI: `ffi_memory_*` and the `cv_ffi::memory` wrapper; C++ allocation hooks; the leak report as log diagnostics | done    |
| M6        | criterion benchmarks, remaining docs                                                      | planned |

There is no separate `StackArena`: a `Bump` with markers is the same thing. Out of scope for now: allocators for the
ECS (archetype chunks) and for the job system (thread-local scratch space); they will be designed together with `ecs`
and `threading`.
