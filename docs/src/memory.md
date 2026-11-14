# Memory

`engine/rust/crates/memory` is the engine's memory layer. It is being built in milestones; this page describes what
exists **today** (milestone M1, the foundation). Arenas, pools and allocation tracking are planned and not
implemented yet.

## What is there

| Item                                         | Purpose                                                                                                                      |
|:---------------------------------------------|:-----------------------------------------------------------------------------------------------------------------------------|
| `Alignment`                                  | A validated power-of-two alignment: `new`, `of::<T>()`, `of_layout`, `align_up`, `align_down`, `is_aligned`, `padding_for`.  |
| `layout_from_parts(size, align)`             | A `Layout` from raw numbers, with every check reported as an error.                                                          |
| `array_layout(element, count)`               | The layout of `count` elements placed like a Rust array (stride = size rounded up to the alignment).                         |
| `ByteSize`                                   | A byte count for budgets and reports: binary units, checked/saturating arithmetic, `Display` (`1.5 KiB`, `3.0 MiB`, ...).    |
| `MemoryError`, `MemoryResult`                | `InvalidAlignment { align }` and `SizeOverflow`. The enum is `#[non_exhaustive]`; later milestones add variants.              |

```rust
use memory::{Alignment, ByteSize, array_layout, layout_from_parts};

let element = layout_from_parts(12, 4)?;          // numbers read from a file header
let block = array_layout(element, 100)?;          // 1200 bytes, alignment 4
assert_eq!(ByteSize::from(block.size()).to_string(), "1.2 KiB");
assert_eq!(Alignment::new(64)?.align_up(100)?, 128);
# Ok::<(), memory::MemoryError>(())
```

## Conventions

* **No panics on bad input.** Sizes and alignments that come from outside are validated and reported as a
  `MemoryError`. `ByteSize` offers checked and saturating arithmetic only, no operators.
* **No `unsafe` yet, no dependencies.** The workspace denies `unsafe_code`. The allocators of the next milestones need
  it; it will be confined to one private module with a safety comment on every block and never appear in the public
  API.
* **Stable Rust.** `allocator_api` is unstable, so allocators cannot be plugged into `Vec<T, A>`. The crate will
  provide its own small container types (arena-backed boxes and vectors) instead of depending on an allocator crate.
* **Single owner.** The arenas planned for M2 are owned by one thread (`Send`, not `Sync`). Only the tracking counters
  of M4 are shared between threads (atomics).

## Roadmap

| Milestone | Content                                                                                             | State   |
|:----------|:----------------------------------------------------------------------------------------------------|:--------|
| M1        | `Alignment`, checked layouts, `ByteSize`, `MemoryError`                                             | done    |
| M2        | `Bump`, `FrameArena` (double-buffered, reset per frame), `StackArena` (markers); the first `unsafe` | planned |
| M3        | `Pool<T>` with `ccore::Handle<T>`: stale handles are rejected                                       | planned |
| M4        | `MemoryTag`, atomic counters, opt-in `TrackingAllocator`, snapshots, budgets, leak report           | planned |
| M5        | FFI: `ffi_memory_stats` and a `cv-ffi` wrapper; optional C++ allocation hooks                       | planned |
| M6        | criterion benchmarks, remaining docs                                                                | planned |

Out of scope for now: allocators for the ECS (archetype chunks) and for the job system (thread-local scratch space).
They will be designed together with `ecs` and `threading`.
