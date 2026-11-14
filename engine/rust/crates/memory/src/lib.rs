//! `memory` — the memory layer of the Coreverse Engine: the building blocks that allocators, arenas, pools and
//! memory tracking are made of.
//!
//! # What is here today
//!
//! The first milestone (M1) is the foundation the later ones stand on. It has no dependencies and no `unsafe`
//! code:
//!
//! * [`Alignment`]: a validated power-of-two alignment with `align_up`, `align_down` and padding arithmetic.
//! * [`layout_from_parts`] and [`array_layout`]: checked construction of [`core::alloc::Layout`] from sizes and
//!   counts that come from outside (files, network, scripts).
//! * [`ByteSize`]: a byte count with binary units, checked and saturating arithmetic and a readable `Display`,
//!   for budgets, limits and statistics.
//! * [`MemoryError`] and [`MemoryResult`].
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
//! # What is planned
//!
//! Arenas (`Bump`, `FrameArena`, `StackArena`), a generational `Pool<T>` on top of `ccore::Handle`, allocation
//! tracking with an opt-in `#[global_allocator]` wrapper, and the FFI counters for C++. Nothing of that exists yet
//! and nothing here is stable API before the crate leaves its early milestones.
//!
//! # Conventions
//!
//! * The workspace denies `unsafe_code`. The allocators of later milestones need it; it will live in one private
//!   module with a safety comment on every block, never in the public API.
//! * Stable Rust only: `allocator_api` is unstable, so allocators will not plug into `Vec<T, A>`; this crate will
//!   provide its own small container types instead.
//! * No function here panics on bad input; sizes and alignments that come from outside are validated and reported
//!   as a [`MemoryError`].

pub mod align;
pub mod error;
pub mod layout;
pub mod size;

pub use align::Alignment;
pub use error::{MemoryError, MemoryResult};
pub use layout::{array_layout, layout_from_parts};
pub use size::ByteSize;
