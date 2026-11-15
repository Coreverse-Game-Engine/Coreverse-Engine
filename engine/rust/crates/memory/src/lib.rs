//! `memory` — the memory layer of the Coreverse Engine: the building blocks that allocators, arenas, pools and
//! memory tracking are made of.
//!
//! # What is here today
//!
//! **M1, the foundation.** No dependencies, no `unsafe` code:
//!
//! * [`Alignment`]: a validated power-of-two alignment with `align_up`, `align_down` and padding arithmetic.
//! * [`layout_from_parts`] and [`array_layout`]: checked construction of [`core::alloc::Layout`] from sizes and
//!   counts that come from outside (files, network, scripts).
//! * [`ByteSize`]: a byte count with binary units, checked and saturating arithmetic and a readable `Display`,
//!   for budgets, limits and statistics.
//! * [`MemoryError`] and [`MemoryResult`].
//!
//! **M2, arenas.** The first (and only) `unsafe` code of the crate, in the private `bump` module:
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
//! # What is planned
//!
//! Allocation tracking with an opt-in `#[global_allocator]` wrapper (M4) and the FFI counters for C++ (M5). None of
//! that exists yet, and nothing here is stable API before the crate leaves its early milestones.
//!
//! # Conventions
//!
//! * The workspace denies `unsafe_code`. It is allowed for the private `bump` module only (with the reason spelled
//!   out), every `unsafe` block there carries a safety comment, and the public API is safe.
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

pub use align::Alignment;
pub use bump::{Bump, Marker};
pub use error::{MemoryError, MemoryResult};
pub use frame::FrameArena;
pub use layout::{array_layout, layout_from_parts};
pub use ccore::Handle;
pub use pool::Pool;
pub use size::ByteSize;
