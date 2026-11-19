//! The event bus: typed handler lists behind one short lock.

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use crate::flow::{Dispatch, Flow};
use crate::priority::EventPriority;
use crate::subscription::Subscription;

/// The handler list of one event type, shared with the `publish` calls that are running. It is replaced, never
/// changed in place, while a `publish` holds it (copy on write through [`Arc::make_mut`]).
type Slots<E> = Arc<Vec<Slot<E>>>;

/// One handler list, with its event type erased. The `TypeId` that keys it names the `Slots<E>` inside.
type Channel = Box<dyn Any + Send + Sync>;

const CHANNEL_MISMATCH: &str = "a channel holds the handler list of its TypeId's event type";

/// Locks the channel map and ignores poisoning.
///
/// Handler code never runs while this lock is held, so a poisoned lock can only come from a bug in the few lines
/// of this module that run under it; those lines leave the map consistent at every point where they could panic.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn slots_ref<E: 'static>(channel: &Channel) -> &Slots<E> {
    channel.downcast_ref::<Slots<E>>().expect(CHANNEL_MISMATCH)
}

fn slots_mut<E: 'static>(channel: &mut Channel) -> &mut Slots<E> {
    channel.downcast_mut::<Slots<E>>().expect(CHANNEL_MISMATCH)
}

/// A handler as the bus stores it.
enum Handler<E> {
    /// Sees the event by shared reference; runs for `publish` and `publish_mut`.
    Observer(Arc<dyn Fn(&E) + Send + Sync>),
    /// May change the event and stop the chain; runs for `publish_mut` only.
    Interceptor(Arc<dyn Fn(&mut E) -> Flow + Send + Sync>),
}

impl<E> Clone for Handler<E> {
    fn clone(&self) -> Self {
        match self {
            Self::Observer(handler) => Self::Observer(Arc::clone(handler)),
            Self::Interceptor(handler) => Self::Interceptor(Arc::clone(handler)),
        }
    }
}

struct Slot<E> {
    id: u64,
    priority: EventPriority,
    /// Cleared when the handler is unsubscribed (or, for a one-shot handler, when it is claimed), so a publish
    /// that already took its list skips the handler.
    active: Arc<AtomicBool>,
    /// A one-shot handler: the first publish that claims it runs it, and no other publish ever does.
    once: bool,
    handler: Handler<E>,
}

impl<E> Clone for Slot<E> {
    fn clone(&self) -> Self {
        Self {
            id: self.id,
            priority: self.priority,
            active: Arc::clone(&self.active),
            once: self.once,
            handler: self.handler.clone(),
        }
    }
}

/// The shared state of a bus and all its clones.
pub(crate) struct Inner {
    next_id: AtomicU64,
    channels: Mutex<HashMap<TypeId, Channel>>,
}

impl Inner {
    fn new() -> Self {
        Self {
            next_id: AtomicU64::new(0),
            channels: Mutex::new(HashMap::new()),
        }
    }

    fn add<E: 'static>(
        &self,
        priority: EventPriority,
        once: bool,
        handler: Handler<E>,
    ) -> (u64, Arc<AtomicBool>) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let active = Arc::new(AtomicBool::new(true));
        let slot = Slot {
            id,
            priority,
            active: Arc::clone(&active),
            once,
            handler,
        };

        let mut channels = lock(&self.channels);
        let channel = channels
            .entry(TypeId::of::<E>())
            .or_insert_with(|| -> Channel { Box::new(Slots::<E>::default()) });
        let slots = Arc::make_mut(slots_mut::<E>(channel));
        // The list is sorted from the highest priority to the lowest; a new handler goes behind every handler of
        // the same or a higher priority, which keeps subscription order inside a priority.
        let at = slots.partition_point(|existing| existing.priority >= priority);
        slots.insert(at, slot);
        (id, active)
    }

    /// Removes a handler; `true` if it was still there.
    pub(crate) fn remove<E: 'static>(&self, id: u64) -> bool {
        let removed = {
            let mut channels = lock(&self.channels);
            let key = TypeId::of::<E>();
            let Some(channel) = channels.get_mut(&key) else {
                return false;
            };
            let slots = slots_mut::<E>(channel);
            let Some(index) = slots.iter().position(|slot| slot.id == id) else {
                return false;
            };
            let removed = Arc::make_mut(slots).remove(index);
            removed.active.store(false, Ordering::Release);
            let now_empty = slots.is_empty();
            if now_empty {
                channels.remove(&key);
            }
            removed
        };
        // The handler is dropped here, after the lock is released: its captures may own a `Subscription` whose
        // drop locks the bus again.
        drop(removed);
        true
    }

    /// Decides whether a publish may run `slot` now.
    ///
    /// An ordinary handler may run while it is active. A one-shot handler may run only for the one publish that
    /// flips its flag: the swap is the whole race, so concurrent publishes cannot both run it. The winner takes
    /// the handler out of the list at once, so a publish started by the handler itself does not see it.
    fn claim<E: 'static>(&self, slot: &Slot<E>) -> bool {
        if !slot.once {
            return slot.active.load(Ordering::Acquire);
        }
        let won = slot.active.swap(false, Ordering::AcqRel);
        if won {
            self.remove::<E>(slot.id);
        }
        won
    }

    fn snapshot<E: 'static>(&self) -> Option<Slots<E>> {
        let channels = lock(&self.channels);
        channels
            .get(&TypeId::of::<E>())
            .map(|channel| Arc::clone(slots_ref::<E>(channel)))
    }

    fn count<E: 'static>(&self) -> usize {
        let channels = lock(&self.channels);
        channels
            .get(&TypeId::of::<E>())
            .map_or(0, |channel| slots_ref::<E>(channel).len())
    }

    fn event_type_count(&self) -> usize {
        lock(&self.channels).len()
    }
}

/// A typed publish/subscribe bus.
///
/// Any `'static` type is an event. [`subscribe`](EventBus::subscribe) registers a handler for one event type and
/// returns a [`Subscription`]; [`publish`](EventBus::publish) calls the handlers of the event's type, on the
/// calling thread, before it returns. Nothing is queued: the queued and cross-thread layers come in later
/// milestones.
///
/// `EventBus` is a handle: cloning it gives another handle to the same bus, and the bus lives until the last
/// handle is dropped. It is `Send + Sync`, so any thread may subscribe, unsubscribe and publish.
///
/// # Rules
///
/// * Handlers of one event run in priority order (see [`EventPriority`]), and in subscription order inside a
///   priority.
/// * No lock is held while a handler runs, so a handler may publish (even the same event type: it recurses),
///   subscribe and unsubscribe.
/// * A `publish` works on the handlers that were subscribed when it began. A handler subscribed during a publish
///   is first called by the next one. A handler unsubscribed during a publish is not called by it, unless it has
///   already started.
/// * A handler that panics unwinds through `publish` (or `publish_mut`) to its caller; the handlers after it are
///   not called for that event. The bus stays usable.
/// * There are two kinds of handler. An *observer* ([`subscribe`](EventBus::subscribe)) sees the event by shared
///   reference. An *interceptor* ([`subscribe_mut`](EventBus::subscribe_mut)) gets `&mut E`, may change the event,
///   and answers with a [`Flow`]: `Stop` ends the chain.
/// * [`publish`](EventBus::publish) takes `&E`, so it runs observers only. [`publish_mut`](EventBus::publish_mut)
///   takes `&mut E` and runs every handler of the type in one chain, in priority order: observers see the event
///   as the interceptors before them left it. Publish an event type the way its handlers expect.
/// * A one-shot handler ([`subscribe_once`](EventBus::subscribe_once)) runs for the first publish that reaches it
///   and is then unsubscribed, even if several threads publish at the same time.
/// * A handler that holds a strong `EventBus` handle that is stored in the same bus keeps the bus alive for ever.
///   Capture a [`WeakEventBus`] ([`EventBus::downgrade`]) instead.
///
/// ```
/// use std::sync::Arc;
/// use std::sync::atomic::{AtomicU32, Ordering};
///
/// use event::EventBus;
///
/// struct Damage(u32);
///
/// let bus = EventBus::new();
/// let total = Arc::new(AtomicU32::new(0));
///
/// let sink = Arc::clone(&total);
/// let _subscription = bus.subscribe(move |hit: &Damage| {
///     sink.fetch_add(hit.0, Ordering::Relaxed);
/// });
///
/// bus.publish(&Damage(7));
/// bus.publish(&Damage(5));
/// assert_eq!(total.load(Ordering::Relaxed), 12);
/// ```
#[derive(Clone)]
pub struct EventBus {
    inner: Arc<Inner>,
}

impl EventBus {
    /// Creates an empty bus.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner::new()),
        }
    }

    /// Subscribes `handler` to events of type `E` with [`EventPriority::Normal`].
    ///
    /// The event type comes from the handler's parameter, so the closure's parameter needs a type
    /// (`|event: &Damage| ...`), or call `subscribe::<Damage, _>(...)`.
    ///
    /// The handler stays subscribed for as long as the returned [`Subscription`] lives.
    ///
    /// ```
    /// use event::EventBus;
    ///
    /// struct Jump;
    ///
    /// let bus = EventBus::new();
    /// let _subscription = bus.subscribe(|_: &Jump| println!("jumped"));
    /// assert_eq!(bus.publish(&Jump), 1);
    /// ```
    pub fn subscribe<E, F>(&self, handler: F) -> Subscription
    where
        E: 'static,
        F: Fn(&E) + Send + Sync + 'static,
    {
        self.subscribe_with_priority(EventPriority::Normal, handler)
    }

    /// Subscribes `handler` to events of type `E` with the given priority.
    ///
    /// ```
    /// use std::sync::{Arc, Mutex};
    ///
    /// use event::{EventBus, EventPriority};
    ///
    /// struct Tick;
    ///
    /// let bus = EventBus::new();
    /// let order = Arc::new(Mutex::new(Vec::new()));
    ///
    /// let low_log = Arc::clone(&order);
    /// let _low = bus.subscribe_with_priority(EventPriority::Low, move |_: &Tick| {
    ///     low_log.lock().unwrap().push("low");
    /// });
    /// let high_log = Arc::clone(&order);
    /// let _high = bus.subscribe_with_priority(EventPriority::High, move |_: &Tick| {
    ///     high_log.lock().unwrap().push("high");
    /// });
    ///
    /// bus.publish(&Tick);
    /// assert_eq!(*order.lock().unwrap(), ["high", "low"]);
    /// ```
    pub fn subscribe_with_priority<E, F>(&self, priority: EventPriority, handler: F) -> Subscription
    where
        E: 'static,
        F: Fn(&E) + Send + Sync + 'static,
    {
        self.register::<E>(priority, false, Handler::Observer(Arc::new(handler)))
    }

    /// Subscribes an interceptor to events of type `E` with [`EventPriority::Normal`].
    ///
    /// An interceptor gets the event by `&mut`, may change it, and returns [`Flow::Continue`] to hand it to the
    /// next handler or [`Flow::Stop`] to end the chain. Interceptors run only for
    /// [`publish_mut`](EventBus::publish_mut); [`publish`](EventBus::publish) cannot lend out `&mut E` and skips
    /// them.
    ///
    /// ```
    /// use event::{EventBus, Flow};
    ///
    /// struct Damage(u32);
    ///
    /// let bus = EventBus::new();
    /// let _armor = bus.subscribe_mut(|damage: &mut Damage| {
    ///     damage.0 /= 2;
    ///     Flow::Continue
    /// });
    ///
    /// let mut hit = Damage(10);
    /// let dispatch = bus.publish_mut(&mut hit);
    /// assert_eq!(hit.0, 5);
    /// assert_eq!(dispatch.called(), 1);
    /// ```
    pub fn subscribe_mut<E, F>(&self, handler: F) -> Subscription
    where
        E: 'static,
        F: Fn(&mut E) -> Flow + Send + Sync + 'static,
    {
        self.subscribe_mut_with_priority(EventPriority::Normal, handler)
    }

    /// Subscribes an interceptor to events of type `E` with the given priority.
    ///
    /// ```
    /// use event::{EventBus, EventPriority, Flow};
    ///
    /// struct Door {
    ///     open: bool,
    /// }
    ///
    /// let bus = EventBus::new();
    /// let _lock = bus.subscribe_mut_with_priority(EventPriority::High, |door: &mut Door| {
    ///     door.open = false;
    ///     Flow::Stop
    /// });
    /// let _opener = bus.subscribe_mut(|door: &mut Door| {
    ///     door.open = true;
    ///     Flow::Continue
    /// });
    ///
    /// let mut door = Door { open: false };
    /// let dispatch = bus.publish_mut(&mut door);
    /// assert!(dispatch.was_stopped());
    /// assert_eq!(dispatch.called(), 1);
    /// assert!(!door.open);
    /// ```
    pub fn subscribe_mut_with_priority<E, F>(
        &self,
        priority: EventPriority,
        handler: F,
    ) -> Subscription
    where
        E: 'static,
        F: Fn(&mut E) -> Flow + Send + Sync + 'static,
    {
        self.register::<E>(priority, false, Handler::Interceptor(Arc::new(handler)))
    }

    /// Subscribes a one-shot observer to events of type `E` with [`EventPriority::Normal`].
    ///
    /// The handler runs for the first event that reaches it and is unsubscribed at that moment. It is an
    /// `FnOnce`, so it may consume what it captured. Dropping the [`Subscription`] before an event arrives
    /// cancels the handler.
    ///
    /// ```
    /// use event::EventBus;
    ///
    /// struct Loaded;
    ///
    /// let bus = EventBus::new();
    /// let _once = bus.subscribe_once(|_: &Loaded| println!("first load"));
    /// assert_eq!(bus.publish(&Loaded), 1);
    /// assert_eq!(bus.publish(&Loaded), 0);
    /// assert_eq!(bus.subscriber_count::<Loaded>(), 0);
    /// ```
    pub fn subscribe_once<E, F>(&self, handler: F) -> Subscription
    where
        E: 'static,
        F: FnOnce(&E) + Send + 'static,
    {
        self.subscribe_once_with_priority(EventPriority::Normal, handler)
    }

    /// Subscribes a one-shot observer to events of type `E` with the given priority.
    pub fn subscribe_once_with_priority<E, F>(
        &self,
        priority: EventPriority,
        handler: F,
    ) -> Subscription
    where
        E: 'static,
        F: FnOnce(&E) + Send + 'static,
    {
        // The bus lets exactly one publish claim a one-shot handler, so the cell is only a way to move the
        // `FnOnce` out of an `Fn` closure; it is never found empty by the publish that claimed the handler.
        let cell = Mutex::new(Some(handler));
        let observer = move |event: &E| {
            let taken = lock(&cell).take();
            if let Some(handler) = taken {
                handler(event);
            }
        };
        self.register::<E>(priority, true, Handler::Observer(Arc::new(observer)))
    }

    fn register<E: 'static>(
        &self,
        priority: EventPriority,
        once: bool,
        handler: Handler<E>,
    ) -> Subscription {
        let (id, active) = self.inner.add::<E>(priority, once, handler);
        Subscription::new(Arc::downgrade(&self.inner), id, active, Inner::remove::<E>)
    }

    /// Calls every observer subscribed to `E`, on this thread, and returns how many it called.
    ///
    /// Interceptors ([`subscribe_mut`](EventBus::subscribe_mut)) are skipped: this method only has `&E`. Use
    /// [`publish_mut`](EventBus::publish_mut) for events that handlers may change or stop.
    ///
    /// See the [type documentation](EventBus) for the order and for what happens when handlers subscribe,
    /// unsubscribe, publish or panic.
    pub fn publish<E: 'static>(&self, event: &E) -> usize {
        let Some(slots) = self.inner.snapshot::<E>() else {
            return 0;
        };
        let mut called = 0;
        for slot in slots.iter() {
            let Handler::Observer(handler) = &slot.handler else {
                continue;
            };
            if !self.inner.claim(slot) {
                continue;
            }
            (**handler)(event);
            called += 1;
        }
        called
    }

    /// Runs every handler subscribed to `E` as one chain, on this thread, and says what happened.
    ///
    /// Handlers run in priority order. An observer sees the event as it is at that point; an interceptor may
    /// change it and may end the chain with [`Flow::Stop`]. The publisher gets the event back, with every
    /// change, when this method returns.
    ///
    /// ```
    /// use event::{EventBus, EventPriority, Flow};
    ///
    /// struct Score(u32);
    ///
    /// let bus = EventBus::new();
    /// let _bonus = bus.subscribe_mut_with_priority(EventPriority::High, |score: &mut Score| {
    ///     score.0 += 100;
    ///     Flow::Continue
    /// });
    /// let _double = bus.subscribe_mut(|score: &mut Score| {
    ///     score.0 *= 2;
    ///     Flow::Continue
    /// });
    ///
    /// let mut score = Score(1);
    /// bus.publish_mut(&mut score);
    /// assert_eq!(score.0, 202);
    /// ```
    pub fn publish_mut<E: 'static>(&self, event: &mut E) -> Dispatch {
        let Some(slots) = self.inner.snapshot::<E>() else {
            return Dispatch::default();
        };
        let mut called = 0;
        let mut stopped = false;
        for slot in slots.iter() {
            if !self.inner.claim(slot) {
                continue;
            }
            called += 1;
            match &slot.handler {
                Handler::Observer(handler) => {
                    let shared: &E = event;
                    (**handler)(shared);
                }
                Handler::Interceptor(handler) => {
                    if (**handler)(&mut *event) == Flow::Stop {
                        stopped = true;
                        break;
                    }
                }
            }
        }
        Dispatch::new(called, stopped)
    }

    /// How many handlers are subscribed to events of type `E` right now.
    pub fn subscriber_count<E: 'static>(&self) -> usize {
        self.inner.count::<E>()
    }

    /// A handle that does not keep the bus alive; what a handler captures when it needs the bus.
    pub fn downgrade(&self) -> WeakEventBus {
        WeakEventBus {
            inner: Arc::downgrade(&self.inner),
        }
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for EventBus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EventBus")
            .field("event_types", &self.inner.event_type_count())
            .finish_non_exhaustive()
    }
}

/// A [`EventBus`] handle that does not keep the bus alive.
///
/// ```
/// use event::EventBus;
///
/// let bus = EventBus::new();
/// let weak = bus.downgrade();
/// assert!(weak.upgrade().is_some());
///
/// drop(bus);
/// assert!(weak.upgrade().is_none());
/// ```
#[derive(Clone, Debug)]
pub struct WeakEventBus {
    inner: Weak<Inner>,
}

impl WeakEventBus {
    /// The bus, if at least one [`EventBus`] handle still exists.
    pub fn upgrade(&self) -> Option<EventBus> {
        self.inner.upgrade().map(|inner| EventBus { inner })
    }
}
