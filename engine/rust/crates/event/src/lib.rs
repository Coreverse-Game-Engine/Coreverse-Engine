//! `event` — the event infrastructure of the Coreverse Engine: a typed publish/subscribe bus that calls handlers
//! synchronously, in a defined order, with subscriptions that clean up after themselves.
//!
//! The crate is built in milestones. This is **E5**, the last one: the synchronous bus (E1), interceptors,
//! cancellable events and one-shot handlers (E2), queued events with a cross-thread sender (E3), delivery as jobs
//! on the `threading` pool (E4), and counters, handler counts and slow-handler reports (E5); the criterion
//! benchmarks are in `benches/event_bench.rs` (see `docs/src/event.md`).
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
//! * Queued events: [`post`](EventBus::post) stores an event, [`dispatch`](EventBus::dispatch) delivers what is
//!   stored, in posting order, at a moment the owner chooses (say, once per frame).
//!   [`EventSender`] ([`EventBus::sender`]) is the cloneable handle that other threads post with; it does not keep
//!   the bus alive and returns a [`PostError`] once the bus is gone.
//! * Parallel delivery on the job system: [`publish_parallel`](EventBus::publish_parallel) runs the observers of
//!   one event as jobs (a priority is a phase: observers of one priority may overlap, the next priority starts
//!   when they are done), and [`dispatch_parallel`](EventBus::dispatch_parallel) delivers the queued events of
//!   different types at the same time while keeping each type's posting order. Both take a
//!   [`threading::Spawner`], which works from any thread, including inside a job.
//! * Diagnostics, all off by default: [`EventBus::set_stats_enabled`] and [`EventBus::stats`] (a [`BusStats`] with
//!   publishes, handler calls, posts, dispatches, the longest queue and slow handlers),
//!   [`EventBus::handler_counts`] (an [`EventTypeInfo`] per event type: observers, interceptors, one-shot
//!   handlers), and [`EventBus::set_slow_handler_hook`], which calls a function with a [`SlowHandler`] report
//!   for every handler that ran longer than a threshold. With the `logging` feature a report becomes a
//!   diagnostic of the logging core (`SlowHandler::to_diagnostic`).
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
//! * `std` and the `threading` crate (plus `log-core` with the optional `logging` feature); no `unsafe` code.
//! * A lock of this crate is never held while handler code runs, so a handler cannot deadlock the bus by what it
//!   does (subscribing, unsubscribing, publishing, or dropping a [`Subscription`]).
//! * A panic in a handler is not caught: it unwinds through [`EventBus::publish`] to its caller.

mod bus;
mod cancellable;
mod diagnostics;
mod flow;
mod priority;
mod queue;
mod sender;
mod subscription;

pub use bus::{EventBus, WeakEventBus};
pub use cancellable::Cancellable;
pub use diagnostics::{BusStats, EventTypeInfo, HandlerKind, SlowHandler};
pub use flow::{Dispatch, Flow};
pub use priority::EventPriority;
pub use sender::{EventSender, PostError};
pub use subscription::Subscription;
