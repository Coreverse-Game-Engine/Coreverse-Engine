//! `utilities` — the small helpers that several Coreverse crates need and that belong to none of them.
//!
//! The crate is deliberately tiny: no dependencies, no `unsafe` code, no global state. It holds only things that
//! were written more than once in the workspace or that every system ends up needing; anything bigger gets a
//! crate of its own. Ids, handles and errors live in `ccore`, memory building blocks in `memory`.
//!
//! * [`sync`]: [`lock`](sync::lock), [`read`](sync::read), [`write`](sync::write) and
//!   [`into_inner`](sync::into_inner), the standard lock operations with poisoning ignored (`threading`, `event`
//!   and `log-sinks` used to carry a private copy of `lock` each; `log-core` and `log-sinks` repeated the
//!   `RwLock` idiom).
//! * [`Defer`] and [`defer`](fn@defer): run a closure when a scope ends, on every path out of it.
//! * [`CachePadded`]: a wrapper that gives a value a cache line of its own, against false sharing.
//! * [`fnv1a_32`] and [`fnv1a_64`]: `const` FNV-1a hashes of bytes, for stable name hashes.
//!
//! ```
//! use std::sync::Mutex;
//!
//! use utilities::sync::lock;
//! use utilities::{defer, fnv1a_64};
//!
//! const PLAYER: u64 = fnv1a_64(b"player");
//!
//! let log = Mutex::new(Vec::new());
//! {
//!     let _done = defer(|| lock(&log).push("done"));
//!     lock(&log).push("working");
//! }
//! assert_eq!(*lock(&log), ["working", "done"]);
//! assert_eq!(PLAYER, fnv1a_64("player".as_bytes()));
//! ```

mod guard;
mod hash;
mod padded;
pub mod sync;

pub use guard::{Defer, defer};
pub use hash::{fnv1a_32, fnv1a_64};
pub use padded::CachePadded;
