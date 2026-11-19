//! `event` — the event infrastructure of the Coreverse Engine: a typed publish/subscribe bus that calls handlers
//! synchronously, in a defined order, with subscriptions that clean up after themselves.
//!
//! The crate is built in milestones. This is **E2**: the synchronous bus (E1) plus interceptors, cancellable
//! events and one-shot handlers. Queued events and the cross-thread layer follow (see `docs/src/event.md`).
//!
//! # What is here today
//!
//! * [`EventBus`]: any `'static` type is an event. [`subscribe`](EventBus::subscribe) /
//!   [`subscribe_with_priority`](EventBus::subscribe_with_priority) register an observer for one event type;
//!   [`publish`](EventBus::publish) calls the observers of that type on the calling thread, before it returns, and
//!   reports how many it called. The bus is a cloneable handle and is `Send + Sync`.
//! * Interceptors: [`subscribe_mut`](EventBus::subscribe_mut) registers a handler that gets `&mut E` and answers
//!   with a [`Flow`] (`Continue` or `Stop`); [`publish_mut`](EventBus::publish_mut) runs the whole chain, observers
//!   and interceptors, and returns a [`Dispatch`] (how many handlers ran, whether the chain was stopped).
//!   [`Cancellable`] wraps an event with a veto flag.
//! * One-shot handlers: [`subscribe_once`](EventBus::subscribe_once) takes an `FnOnce`, runs it for the first event
//!   and unsubscribes it.
//! * [`Subscription`]: what `subscribe` returns. **Dropping it unsubscribes the handler**;
//!   [`detach`](Subscription::detach) keeps the handler for the life of the bus.
//! * [`EventPriority`]: `High`, `Normal` (default) and `Low`. Same priority: subscription order.
//! * [`WeakEventBus`] ([`EventBus::downgrade`]): a handle that does not keep the bus alive, for handlers that need
//!   the bus.
//!
//! ```
//! use event::EventBus;
//!
//! struct WindowResized {
//!     width: u32,
//!     height: u32,
//! }
//!
//! let bus = EventBus::new();
//! let _subscription = bus.subscribe(|resized: &WindowResized| {
//!     assert_eq!((resized.width, resized.height), (1280, 720));
//! });
//!
//! assert_eq!(bus.publish(&WindowResized { width: 1280, height: 720 }), 1);
//! ```
//!
//! # Conventions
//!
//! * `std` only and no dependencies; no `unsafe` code.
//! * A lock of this crate is never held while handler code runs, so a handler cannot deadlock the bus by what it
//!   does (subscribing, unsubscribing, publishing, or dropping a [`Subscription`]).
//! * A panic in a handler is not caught: it unwinds through [`EventBus::publish`] to its caller.

mod bus;
mod cancellable;
mod flow;
mod priority;
mod subscription;

pub use bus::{EventBus, WeakEventBus};
pub use cancellable::Cancellable;
pub use flow::{Dispatch, Flow};
pub use priority::EventPriority;
pub use subscription::Subscription;
