//! `memory` — the memory layer of the Coreverse Engine: the building blocks that allocators, arenas, pools and
//! memory tracking are made of.
//!
//! # What is here today
//!
//! **M1, the foundation.** No `unsafe` code, nothing that needs a dependency:
//!
//! * [`Alignment`]: a validated power-of-two alignment with `align_up`, `align_down` and padding arithmetic.
//! * [`layout_from_parts`] and [`array_layout`]: checked construction of [`core::alloc::Layout`] from sizes and
//!   counts that come from outside (files, network, scripts).
//! * [`ByteSize`]: a byte count with binary units, checked and saturating arithmetic and a readable `Display`,
//!   for budgets, limits and statistics.
//! * [`MemoryError`] and [`MemoryResult`].
//!
//! **M2, arenas.** The first `unsafe` code of the crate, in the private `bump` module:
//!
//! * [`Bump`]: a fixed-capacity bump allocator for `Copy` values. Allocating takes `&self` and returns a reference;
//!   freeing ([`Bump::reset`], [`Bump::rewind`] to a [`Marker`]) takes `&mut self`, so the compiler rejects every use
//!   of memory that has been given back.
//! * [`FrameArena`]: a `Bump` that is emptied once per frame, with a frame counter and the previous frame's usage.
//!
//! ```
//! use memory::{Alignment, ByteSize};
//!
//! let cache_line = Alignment::new(64)?;
//! assert_eq!(cache_line.align_up(100)?, 128);
//! assert_eq!(ByteSize::from_mib(3).to_string(), "3.0 MiB");
//! # Ok::<(), memory::MemoryError>(())
//! ```
//!
//! ```
//! use memory::Bump;
//!
//! let mut scratch = Bump::with_capacity(4096)?;
//! let name = scratch.alloc_slice_copy(b"player")?;
//! let health = scratch.alloc(100u32)?;
//! *health -= 25;
//! assert_eq!((name.len(), *health), (6, 75));
//! scratch.reset(); // `name` and `health` can no longer be used
//! # Ok::<(), memory::MemoryError>(())
//! ```
//!
//! **M3, the pool.** Entirely safe code:
//!
//! * [`Pool`]: stores values and addresses them by [`ccore::Handle`] (slot index plus generation). Removing a value
//!   makes every old handle to it stale, even after the slot is reused; a stale or invented handle finds nothing.
//!
//! ```
//! use memory::Pool;
//!
//! let mut pool = Pool::new();
//! let handle = pool.insert("hello")?;
//! assert_eq!(pool.get(handle), Some(&"hello"));
//! pool.remove(handle);
//! assert_eq!(pool.get(handle), None);
//! # Ok::<(), memory::MemoryError>(())
//! ```
//!
//! **M4, tracking.** Memory is counted in two independent ways:
//!
//! * [`MemoryTracker`] counts memory that was *registered* under a [`MemoryTag`] (the engine system it belongs to):
//!   [`Bump::with_capacity_tagged`] and [`FrameArena::with_capacity_tagged`] register their blocks in the
//!   [`global_tracker`]. It supports an advisory budget per tag and a [`MemoryReport`] whose
//!   [`leaks`](MemoryReport::leaks) lists the tags that still have bytes in use at shutdown.
//! * [`TrackingAllocator`] wraps the global allocator and counts every Rust allocation of the process (bytes in
//!   use, peak, totals, number of calls) without knowing the system an allocation belongs to. Nothing installs it;
//!   a binary opts in with `#[global_allocator]`.
//!
//! ```
//! use memory::{ByteSize, Bump, MemoryTag, global_tracker};
//!
//! global_tracker().set_budget(MemoryTag::Rendering, Some(ByteSize::from_mib(64)));
//! let frame_memory = Bump::with_capacity_tagged(1 << 20, MemoryTag::Rendering)?;
//! assert!(!global_tracker().is_over_budget(MemoryTag::Rendering));
//! assert!(global_tracker().report().has_leaks()); // still alive: would be a leak at shutdown
//! drop(frame_memory);
//! assert!(!global_tracker().report().has_leaks());
//! # Ok::<(), memory::MemoryError>(())
//! ```
//!
//! **M5, the C++ side.** Not in this crate: the `ffi` crate exposes the [`global_tracker`] to C++ (`ffi_memory_*`) and
//! turns the [`MemoryReport`] into log diagnostics, and `cv-ffi` wraps it in `cv_ffi::memory` (`TrackedAllocation`, a
//! `std::pmr` resource that registers its allocations). This crate does not depend on either.
//!
//! **M6, benchmarks.** `cargo bench -p memory` (criterion): the arenas against the heap, the pool, the counters and
//! the overhead of the [`TrackingAllocator`].
//!
//! # What is not here
//!
//! Allocators for the ECS (archetype chunks) and for the job system (thread-local scratch space), a double-buffered
//! frame arena and arenas that grow: nothing needs them yet, and each would change the design (see the page on
//! memory in the book). The API of this crate may still change while the systems that use it are written.
//!
//! # Conventions
//!
//! * The workspace denies `unsafe_code`. It is allowed for the private `bump` and `tracking` modules only (with the
//!   reason spelled out), every `unsafe` block there carries a safety comment, and the public API is safe.
//! * Stable Rust only: `allocator_api` is unstable, so allocators do not plug into `Vec<T, A>`; the arenas hand out
//!   references and slices instead.
//! * No function here panics on bad input; sizes and alignments that come from outside are validated and reported
//!   as a [`MemoryError`].

pub mod align;
#[allow(
    unsafe_code,
    reason = "the bump allocator hands out references into a block it owns; every unsafe block documents why it is sound, and the public API is safe"
)]
mod bump;
pub mod error;
pub mod frame;
pub mod layout;
pub mod pool;
pub mod size;
pub mod stats;
pub mod tag;
pub mod tracker;
#[allow(
    unsafe_code,
    reason = "implementing `GlobalAlloc` is `unsafe`; the impl only forwards to the wrapped allocator and counts with atomics, and every unsafe block documents why it is sound"
)]
mod tracking;

pub use align::Alignment;
pub use bump::{Bump, Marker};
pub use ccore::Handle;
pub use error::{MemoryError, MemoryResult};
pub use frame::FrameArena;
pub use layout::{array_layout, layout_from_parts};
pub use pool::Pool;
pub use size::ByteSize;
pub use stats::MemoryStats;
pub use tag::MemoryTag;
pub use tracker::{MemoryReport, MemoryTracker, TagReport, global_tracker};
pub use tracking::TrackingAllocator;
