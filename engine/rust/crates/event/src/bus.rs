//! The event bus: typed handler lists behind one short lock.

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use crate::priority::EventPriority;
use crate::subscription::Subscription;

/// A handler as the bus stores it.
type Handler<E> = Arc<dyn Fn(&E) + Send + Sync>;

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

struct Slot<E> {
    id: u64,
    priority: EventPriority,
    /// Cleared when the handler is unsubscribed, so a `publish` that already took its list skips the handler.
    active: Arc<AtomicBool>,
    handler: Handler<E>,
}

impl<E> Clone for Slot<E> {
    fn clone(&self) -> Self {
        Self {
            id: self.id,
            priority: self.priority,
            active: Arc::clone(&self.active),
            handler: Arc::clone(&self.handler),
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
        handler: Handler<E>,
    ) -> (u64, Arc<AtomicBool>) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let active = Arc::new(AtomicBool::new(true));
        let slot = Slot {
            id,
            priority,
            active: Arc::clone(&active),
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
/// * A handler that panics unwinds through `publish` to its caller; the handlers after it are not called for
///   that event. The bus stays usable.
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
        let (id, active) = self.inner.add::<E>(priority, Arc::new(handler));
        Subscription::new(Arc::downgrade(&self.inner), id, active, Inner::remove::<E>)
    }

    /// Calls every handler subscribed to `E`, on this thread, and returns how many it called.
    ///
    /// See the [type documentation](EventBus) for the order and for what happens when handlers subscribe,
    /// unsubscribe, publish or panic.
    pub fn publish<E: 'static>(&self, event: &E) -> usize {
        let Some(slots) = self.inner.snapshot::<E>() else {
            return 0;
        };
        let mut called = 0;
        for slot in slots.iter() {
            if slot.active.load(Ordering::Acquire) {
                (slot.handler)(event);
                called += 1;
            }
        }
        called
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
