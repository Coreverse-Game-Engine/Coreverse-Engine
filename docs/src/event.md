# Events

`engine/rust/crates/event` is the engine's event infrastructure. It is being built in milestones; this page describes
what exists (E1, the synchronous bus) and what is planned. The crate uses `std` only, has no dependencies and no
`unsafe` code.

## What is there (E1)

| Item            | Purpose                                                                                                                                   |
|:----------------|:------------------------------------------------------------------------------------------------------------------------------------------|
| `EventBus`      | A typed publish/subscribe bus. Any `'static` type is an event. `subscribe(handler)`, `subscribe_with_priority(priority, handler)`, `publish(&event)`, `subscriber_count::<E>()`, `downgrade()`. A cloneable handle, `Send + Sync`. |
| `Subscription`  | What `subscribe` returns. **Dropping it unsubscribes the handler.** `unsubscribe()`, `detach()` (keep the handler for the life of the bus), `is_active()`, `id()`. |
| `EventPriority` | `High`, `Normal` (default), `Low`. A higher priority always runs first; the same priority runs in subscription order.                    |
| `WeakEventBus`  | A handle that does not keep the bus alive; what a handler captures when it needs the bus. `upgrade()`.                                    |

```rust,ignore
use event::{EventBus, EventPriority};

struct WindowResized { width: u32, height: u32 }

let bus = EventBus::new();

// The handler lives as long as the subscription does.
let subscription = bus.subscribe(|resized: &WindowResized| {
    println!("{}x{}", resized.width, resized.height);
});

// Priorities: High runs first. Same priority: in the order of subscription.
let _first = bus.subscribe_with_priority(EventPriority::High, |_: &WindowResized| {});

// `publish` calls the handlers on this thread, before it returns, and says how many it called.
assert_eq!(bus.publish(&WindowResized { width: 1280, height: 720 }), 2);

drop(subscription); // unsubscribed
assert_eq!(bus.subscriber_count::<WindowResized>(), 1);
```

## Rules

* **Synchronous.** `publish` runs the handlers on the calling thread and returns when the last one is done. Nothing is
  queued in E1.
* **No lock is held while a handler runs.** A handler may publish (even the same event type; it recurses), subscribe,
  unsubscribe, and drop `Subscription`s, including the one of a handler that is being removed.
* **A publish works on the handlers that existed when it began.** A handler subscribed during a publish is first called
  by the next publish. A handler unsubscribed during a publish is not called by it, unless it has already started.
* **A panicking handler is not caught.** It unwinds through `publish`; the handlers behind it are not called for that
  event; the bus stays usable (its lock is only held for list updates).
* **Cycles.** A handler that captures a strong `EventBus` handle which is stored in the same bus keeps it alive for
  ever. Capture `bus.downgrade()` and `upgrade()` inside the handler.
* **Cost.** `publish` takes the bus lock for one map lookup and one `Arc` clone, then runs without the lock and
  without allocating. `subscribe` and `unsubscribe` copy the handler list of that event type when a publish is
  running at the same moment, and update it in place otherwise.

## Why these choices

* **`'static` types instead of an `Event` trait.** No derive or impl is needed to make something an event, and the
  reflection and serialization crates (planned) can describe events like any other type later.
* **Drop unsubscribes.** The usual bug in an event system is a handler that outlives the object it points at. Tying
  the handler to a value the owner holds removes that class of bug; `detach()` is there for the rare handler that
  really lives for the whole program.
* **Copy on write handler lists.** A running publish keeps its own list, so unsubscribing and subscribing never
  have to wait for handlers and handlers never see a list change under them.

## Planned

| Milestone | Scope                                                                                                                                                                    |
|:----------|:-------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| E1        | Synchronous typed bus, priorities, subscription handles, thread-safe, re-entrancy-safe (this page).                                                                      |
| E2        | Cancellable and mutable events (`publish_mut`, a handler result that stops the chain), one-shot handlers.                                                                |
| E3        | Queued events: `post` now, `dispatch` at a chosen point (for example once per frame); a multi-producer channel for messages between threads.                            |
| E4        | `threading` integration: dispatch a queue's handlers as jobs. Starts after the `threading` crate's task graphs (T4).                                                     |
| E5        | Diagnostics (handler counts, slow-handler reporting through the logger), benchmarks, FFI exposure if the C++ side needs it.                                              |
