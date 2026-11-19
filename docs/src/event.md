# Events

`engine/rust/crates/event` is the engine's event infrastructure. It is being built in milestones; this page describes
what exists (E1, the synchronous bus, and E2, interceptors, cancellable events and one-shot handlers) and what is
planned. The crate uses `std` only, has no dependencies and no `unsafe` code.

## What is there

| Item            | Purpose                                                                                                                                                          |
|:----------------|:-----------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `EventBus`      | A typed publish/subscribe bus. Any `'static` type is an event. A cloneable handle, `Send + Sync`. See the table below for its methods.                          |
| `Subscription`  | What every `subscribe*` method returns. **Dropping it unsubscribes the handler.** `unsubscribe()`, `detach()` (keep the handler for the life of the bus), `is_active()`, `id()`. |
| `EventPriority` | `High`, `Normal` (default), `Low`. A higher priority always runs first; the same priority runs in subscription order.                                           |
| `WeakEventBus`  | A handle that does not keep the bus alive; what a handler captures when it needs the bus. `upgrade()`.                                                          |
| `Flow`          | What an interceptor answers: `Continue` (next handler) or `Stop` (end the chain).                                                                               |
| `Dispatch`      | What `publish_mut` returns: `called()` (how many handlers ran) and `was_stopped()`.                                                                              |
| `Cancellable<E>`| An event with a veto flag: `new`, `cancel`, `is_cancelled`, `into_inner`; dereferences to `E`.                                                                  |

| Method                                                    | Handler gets | Runs for                         | Notes                                                       |
|:----------------------------------------------------------|:-------------|:---------------------------------|:------------------------------------------------------------|
| `subscribe`, `subscribe_with_priority`                    | `&E`         | `publish`, `publish_mut`         | An *observer*. Returns nothing.                             |
| `subscribe_mut`, `subscribe_mut_with_priority`            | `&mut E`     | `publish_mut` only               | An *interceptor*. Returns a `Flow`.                         |
| `subscribe_once`, `subscribe_once_with_priority`          | `&E`         | `publish`, `publish_mut`         | An `FnOnce` observer; runs for the first event it receives. |
| `publish(&E) -> usize`                                    |              |                                  | Runs the observers; returns how many ran.                   |
| `publish_mut(&mut E) -> Dispatch`                         |              |                                  | Runs the whole chain: observers and interceptors.           |
| `subscriber_count::<E>()`, `downgrade()`                  |              |                                  | All kinds of handler count.                                 |

```rust,ignore
use event::{Cancellable, EventBus, EventPriority, Flow};

struct WindowResized { width: u32, height: u32 }
struct Damage(u32);
struct Move { to: (i32, i32) }

let bus = EventBus::new();

// An observer. It lives as long as the subscription does.
let subscription = bus.subscribe(|resized: &WindowResized| {
    println!("{}x{}", resized.width, resized.height);
});
assert_eq!(bus.publish(&WindowResized { width: 1280, height: 720 }), 1);
drop(subscription); // unsubscribed

// An interceptor changes the event; `publish_mut` hands the changed event back to the publisher.
let _armor = bus.subscribe_mut(|damage: &mut Damage| {
    damage.0 /= 2;
    Flow::Continue
});
let mut hit = Damage(10);
bus.publish_mut(&mut hit);
assert_eq!(hit.0, 5);

// A veto: handlers cancel, the publisher decides what that means.
let _wall = bus.subscribe_mut(|attempt: &mut Cancellable<Move>| {
    if attempt.to.0 < 0 {
        attempt.cancel();
    }
    Flow::Continue
});
let mut attempt = Cancellable::new(Move { to: (-1, 0) });
bus.publish_mut(&mut attempt);
assert!(attempt.is_cancelled());

// A one-shot handler runs for the first event and is then unsubscribed.
let _first = bus.subscribe_once(|_: &WindowResized| println!("first resize"));
```

## Rules

* **Synchronous.** `publish` and `publish_mut` run the handlers on the calling thread and return when the last one is
  done. Nothing is queued yet.
* **No lock is held while a handler runs.** A handler may publish (even the same event type; it recurses), subscribe,
  unsubscribe, and drop `Subscription`s, including the one of a handler that is being removed.
* **A publish works on the handlers that existed when it began.** A handler subscribed during a publish is first
  called by the next publish. A handler unsubscribed during a publish is not called by it, unless it has already
  started.
* **Observers and interceptors.** `publish(&E)` only has a shared reference, so it runs observers and skips
  interceptors. `publish_mut(&mut E)` runs every handler of the type as one chain in priority order: an observer
  sees the event as the interceptors before it left it. Publish an event type the way its handlers expect; a type
  whose handlers are interceptors is meant for `publish_mut`.
* **Stopping.** An interceptor that returns `Flow::Stop` ends the chain: no handler after it runs, observers
  included. `Dispatch::called()` counts the handler that stopped it, `Dispatch::was_stopped()` says it happened.
  Only interceptors can stop; an observer that must be able to stop the chain is registered as an interceptor.
* **Cancelling is not stopping.** `Cancellable::cancel` sets a flag that later handlers and the publisher can read.
  It does not end the chain.
* **One-shot handlers run exactly once**, even when several threads publish at the same moment: the first publish
  that reaches the handler claims it with one atomic operation and takes it out of the list before running it, so
  a publish started by the handler itself does not see it. `Subscription::is_active()` is `false` from that moment.
  Dropping the `Subscription` earlier cancels the handler. A one-shot handler that panics is already gone.
* **A panicking handler is not caught.** It unwinds through `publish`/`publish_mut`; the handlers behind it are not
  called for that event; changes that interceptors before it made stay in the event; the bus stays usable (its lock
  is only held for list updates).
* **Cycles.** A handler that captures a strong `EventBus` handle which is stored in the same bus keeps it alive for
  ever. Capture `bus.downgrade()` and `upgrade()` inside the handler.
* **Cost.** `publish` takes the bus lock for one map lookup and one `Arc` clone, then runs without the lock and
  without allocating. `subscribe` and `unsubscribe` copy the handler list of that event type when a publish is
  running at the same moment, and update it in place otherwise. A one-shot handler costs one extra lock round trip
  when it is claimed.

## Why these choices

* **`'static` types instead of an `Event` trait.** No derive or impl is needed to make something an event, and the
  reflection and serialization crates (planned) can describe events like any other type later.
* **Drop unsubscribes.** The usual bug in an event system is a handler that outlives the object it points at. Tying
  the handler to a value the owner holds removes that class of bug; `detach()` is there for the rare handler that
  really lives for the whole program.
* **Copy on write handler lists.** A running publish keeps its own list, so unsubscribing and subscribing never
  have to wait for handlers and handlers never see a list change under them.
* **Two kinds of handler, not one with `&mut`.** Most handlers only look at an event; giving all of them `&mut E`
  would make every publisher hold an exclusive reference and would stop events from being shared between threads
  or published from `&self` code. Interceptors are the opt-in for events that are meant to be changed.
* **`Cancellable` is a wrapper, not a bus feature.** The bus only knows `Flow`. What "cancelled" means differs
  between a physics step, a UI click and a file save, so the flag lives in the event and the publisher interprets it.
* **No one-shot interceptors.** `subscribe_once` is for observers. A one-shot veto is rare; it is a `subscribe_mut`
  handler that unsubscribes itself through its `Subscription`.

## Planned

| Milestone | Scope                                                                                                                                                                    |
|:----------|:-------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| E1        | Synchronous typed bus, priorities, subscription handles, thread-safe, re-entrancy-safe. Done.                                                                            |
| E2        | Interceptors (`subscribe_mut`, `publish_mut`, `Flow`, `Dispatch`), `Cancellable`, one-shot handlers (`subscribe_once`). Done (this page).                                |
| E3        | Queued events: `post` now, `dispatch` at a chosen point (for example once per frame); a multi-producer channel for messages between threads.                            |
| E4        | `threading` integration: dispatch a queue's handlers as jobs. Starts after the `threading` crate's task graphs (T4).                                                     |
| E5        | Diagnostics (handler counts, slow-handler reporting through the logger), benchmarks, FFI exposure if the C++ side needs it.                                              |
