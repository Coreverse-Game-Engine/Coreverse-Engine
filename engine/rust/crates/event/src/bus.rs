//! The event bus: typed handler lists behind one short lock.

use std::any::{Any, TypeId, type_name};
use std::collections::HashMap;
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use threading::Spawner;

use crate::diagnostics::{
    BusStats, EventTypeInfo, HandlerKind, Probe, Site, SlowConfig, SlowHandler, Stats,
};
use crate::flow::{Dispatch, Flow};
use crate::priority::EventPriority;
use crate::queue::{Queue, Queued, queued};
use crate::sender::EventSender;
use crate::subscription::Subscription;

/// The handler list of one event type, shared with the `publish` calls that are running. It is replaced, never
/// changed in place, while a `publish` holds it (copy on write through [`Arc::make_mut`]).
type Slots<E> = Arc<Vec<Slot<E>>>;

/// How many observers, interceptors and one-shot handlers a channel holds.
type Summary = (usize, usize, usize);

/// One handler list, with its event type erased. The `TypeId` that keys it names the `Slots<E>` inside `slots`.
struct Channel {
    /// The name of the event type, for diagnostics.
    name: &'static str,
    slots: Box<dyn Any + Send + Sync>,
    /// Counts the handlers of this channel; instantiated for the event type when the channel is made.
    summary: fn(&Channel) -> Summary,
}

const CHANNEL_MISMATCH: &str = "a channel holds the handler list of its TypeId's event type";

/// Locks the channel map and ignores poisoning.
///
/// Handler code never runs while this lock is held, so a poisoned lock can only come from a bug in the few lines
/// of this module that run under it; those lines leave the map consistent at every point where they could panic.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn slots_ref<E: 'static>(channel: &Channel) -> &Slots<E> {
    channel
        .slots
        .downcast_ref::<Slots<E>>()
        .expect(CHANNEL_MISMATCH)
}

fn slots_mut<E: 'static>(channel: &mut Channel) -> &mut Slots<E> {
    channel
        .slots
        .downcast_mut::<Slots<E>>()
        .expect(CHANNEL_MISMATCH)
}

/// Counts the handlers of the channel of event type `E`.
fn summarize<E: 'static>(channel: &Channel) -> Summary {
    let mut observers = 0;
    let mut interceptors = 0;
    let mut one_shot = 0;
    for slot in slots_ref::<E>(channel).iter() {
        match &slot.handler {
            Handler::Observer(_) => observers += 1,
            Handler::Interceptor(_) => interceptors += 1,
        }
        if slot.once {
            one_shot += 1;
        }
    }
    (observers, interceptors, one_shot)
}

/// Describes the handler `slot` for the counters and the slow-handler reports.
fn site<E: 'static>(slot: &Slot<E>, kind: HandlerKind) -> Site {
    Site {
        event_type: type_name::<E>(),
        id: slot.id,
        priority: slot.priority,
        kind,
    }
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
    /// The events posted and not yet dispatched.
    pub(crate) queue: Queue,
    /// The counters; they run only while `stats.enabled` is set.
    stats: Stats,
    /// Set while a slow-handler hook is installed, so that a publish can skip the lock below.
    slow_enabled: AtomicBool,
    slow: Mutex<Option<SlowConfig>>,
}

impl Inner {
    fn new() -> Self {
        Self {
            next_id: AtomicU64::new(0),
            channels: Mutex::new(HashMap::new()),
            queue: Queue::new(),
            stats: Stats::default(),
            slow_enabled: AtomicBool::new(false),
            slow: Mutex::new(None),
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
            .or_insert_with(|| Channel {
                name: type_name::<E>(),
                slots: Box::new(Slots::<E>::default()),
                summary: summarize::<E>,
            });
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

    /// One entry per event type that has at least one handler, sorted by name.
    fn event_types(&self) -> Vec<EventTypeInfo> {
        let mut infos: Vec<EventTypeInfo> = {
            let channels = lock(&self.channels);
            channels
                .values()
                .map(|channel| {
                    let (observers, interceptors, one_shot) = (channel.summary)(channel);
                    EventTypeInfo::new(channel.name, observers, interceptors, one_shot)
                })
                .collect()
        };
        infos.sort_by_key(EventTypeInfo::name);
        infos
    }

    /// What a publish that starts now has to measure: nothing, by default.
    fn probe(&self) -> Probe<'_> {
        let stats = if self.stats.enabled.load(Ordering::Relaxed) {
            Some(&self.stats)
        } else {
            None
        };
        let slow = if self.slow_enabled.load(Ordering::Acquire) {
            lock(&self.slow).clone()
        } else {
            None
        };
        Probe::new(stats, slow)
    }

    /// Queues an event and updates the queue counters.
    pub(crate) fn enqueue(&self, item: Queued) {
        let pending = self.queue.push(item);
        if self.stats.enabled.load(Ordering::Relaxed) {
            self.stats.posted.fetch_add(1, Ordering::Relaxed);
            let pending = u64::try_from(pending).unwrap_or(u64::MAX);
            self.stats.max_pending.fetch_max(pending, Ordering::Relaxed);
        }
    }

    /// Counts one queued event that a dispatch pass took up.
    fn count_dispatched(&self) {
        if self.stats.enabled.load(Ordering::Relaxed) {
            self.stats.dispatched.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// A typed publish/subscribe bus.
///
/// Any `'static` type is an event. [`subscribe`](EventBus::subscribe) registers a handler for one event type and
/// returns a [`Subscription`]; [`publish`](EventBus::publish) calls the handlers of the event's type, on the
/// calling thread, before it returns. [`post`](EventBus::post) queues an event instead, and
/// [`dispatch`](EventBus::dispatch) delivers the queued events at a moment the owner of the bus chooses (for
/// example once per frame); an [`EventSender`] lets other threads post.
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
        let probe = self.inner.probe();
        probe.publish_started();
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
            probe.call(site::<E>(slot, HandlerKind::Observer), || {
                (**handler)(event);
            });
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
        let probe = self.inner.probe();
        probe.publish_started();
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
                    probe.call(site::<E>(slot, HandlerKind::Observer), || {
                        (**handler)(shared);
                    });
                }
                Handler::Interceptor(handler) => {
                    let flow = probe.call(site::<E>(slot, HandlerKind::Interceptor), || {
                        (**handler)(&mut *event)
                    });
                    if flow == Flow::Stop {
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

    /// Queues `event` to be delivered by the next [`dispatch`](EventBus::dispatch). Nothing runs now.
    ///
    /// Any thread may post, so the event must be `Send`. Events are delivered in the order in which they were
    /// posted, whatever their types; events of one thread keep that thread's order. The handlers that exist when
    /// the event is *dispatched* receive it, not the ones that existed when it was posted.
    ///
    /// A queued event is owned by the queue, so it is delivered like [`publish_mut`](EventBus::publish_mut): every
    /// handler of its type runs, observers, interceptors and one-shot handlers alike, and `Flow::Stop` ends the
    /// chain. Nobody is left to read what interceptors change.
    ///
    /// The queue is unbounded; a producer that outpaces `dispatch` makes it grow.
    ///
    /// ```
    /// use std::sync::{Arc, Mutex};
    ///
    /// use event::EventBus;
    ///
    /// struct Spawned(u32);
    ///
    /// let bus = EventBus::new();
    /// let seen = Arc::new(Mutex::new(Vec::new()));
    /// let sink = Arc::clone(&seen);
    /// let _subscription = bus.subscribe(move |spawned: &Spawned| {
    ///     sink.lock().unwrap().push(spawned.0);
    /// });
    ///
    /// bus.post(Spawned(1));
    /// bus.post(Spawned(2));
    /// assert!(seen.lock().unwrap().is_empty(), "nothing has run yet");
    /// assert_eq!(bus.pending(), 2);
    ///
    /// assert_eq!(bus.dispatch(), 2); // for example once per frame
    /// assert_eq!(*seen.lock().unwrap(), [1, 2]);
    /// ```
    ///
    /// An event that is not `Send` cannot be posted:
    ///
    /// ```compile_fail
    /// use std::rc::Rc;
    ///
    /// use event::EventBus;
    ///
    /// let bus = EventBus::new();
    /// bus.post(Rc::new(1));
    /// ```
    pub fn post<E: Send + 'static>(&self, event: E) {
        self.inner.enqueue(queued(event));
    }

    /// Delivers the events that are queued right now, in posting order, on this thread, and returns how many it
    /// delivered (an event nobody listens to counts too).
    ///
    /// * **One pass.** Events posted while the pass runs, by handlers or by other threads, wait for the next
    ///   call. Call `dispatch` again, for example once per frame, to deliver them.
    /// * **One pass at a time.** If a pass is already running, on another thread or because a handler called
    ///   `dispatch` while being dispatched, this call returns 0 at once and leaves the queue to the running pass.
    ///   Events are therefore never delivered concurrently or out of order.
    /// * **A panicking handler loses only its own event.** The panic unwinds through `dispatch`; the events of the
    ///   pass that were not delivered yet go back to the front of the queue, in order, and the next call delivers
    ///   them.
    pub fn dispatch(&self) -> usize {
        let Some(mut pass) = self.inner.queue.begin_pass() else {
            return 0;
        };
        let mut delivered = 0;
        while let Some(item) = pass.pop() {
            item.deliver(self);
            self.inner.count_dispatched();
            delivered += 1;
        }
        delivered
    }

    /// Like [`publish`](EventBus::publish), but runs the observers as jobs on the thread pool behind `spawner`,
    /// and returns how many it called when all of them are done.
    ///
    /// * **Priorities are phases.** Observers of the same priority may run at the same time, in no particular
    ///   order; every observer of a higher priority has finished before the first one of the next priority
    ///   starts. (`publish` runs one observer after the other in subscription order; this method does not
    ///   promise that order inside a priority.)
    /// * **Observers only.** Interceptors need `&mut E` in a fixed order and are skipped, as in `publish`.
    ///   The event is shared by the handlers that run at the same time, so it must be `Sync`.
    /// * **One-shot handlers** still run exactly once, and unsubscribing, subscribing and the snapshot rules of
    ///   [`EventBus`] are unchanged.
    /// * **A panicking observer** does not stop the other observers of its phase; the first panic is re-raised
    ///   when the phase is done, and the phases after it do not run (the threading crate's rule for scopes).
    /// * **Where it can be called from.** A [`Spawner`] works anywhere, including inside a job; a thread that
    ///   waits for the phase runs queued jobs meanwhile, so calling this from a job cannot starve a small pool.
    ///   With a pool without workers (`WorkerCount::Exact(0)`) the observers run on the calling thread.
    ///
    /// Running a handler as a job costs more than calling it: use this for a few expensive observers, not for
    /// many cheap ones.
    ///
    /// ```
    /// use std::sync::Arc;
    /// use std::sync::atomic::{AtomicUsize, Ordering};
    ///
    /// use event::EventBus;
    /// use threading::{PoolConfig, ThreadPool};
    ///
    /// struct Frame(u32);
    ///
    /// let pool = ThreadPool::new(PoolConfig::new())?;
    /// let bus = EventBus::new();
    /// let seen = Arc::new(AtomicUsize::new(0));
    ///
    /// let subscriptions: Vec<_> = (0..4)
    ///     .map(|_| {
    ///         let seen = Arc::clone(&seen);
    ///         bus.subscribe(move |_: &Frame| {
    ///             seen.fetch_add(1, Ordering::SeqCst);
    ///         })
    ///     })
    ///     .collect();
    ///
    /// assert_eq!(bus.publish_parallel(&pool.spawner(), &Frame(1)), 4);
    /// assert_eq!(seen.load(Ordering::SeqCst), 4);
    /// drop(subscriptions);
    /// # Ok::<(), threading::ThreadingError>(())
    /// ```
    ///
    /// An event that is not `Sync` cannot be shared between jobs:
    ///
    /// ```compile_fail
    /// use std::cell::Cell;
    ///
    /// use event::EventBus;
    /// use threading::{PoolConfig, ThreadPool};
    ///
    /// let pool = ThreadPool::new(PoolConfig::new()).unwrap();
    /// let bus = EventBus::new();
    /// bus.publish_parallel(&pool.spawner(), &Cell::new(1));
    /// ```
    pub fn publish_parallel<E: Sync + 'static>(&self, spawner: &Spawner, event: &E) -> usize {
        let probe = self.inner.probe();
        probe.publish_started();
        let Some(slots) = self.inner.snapshot::<E>() else {
            return 0;
        };
        let mut called = 0;
        let mut start = 0;
        while start < slots.len() {
            // The list is sorted by priority, so a phase is a run of slots of the same priority.
            let priority = slots[start].priority;
            let length = slots[start..]
                .iter()
                .take_while(|slot| slot.priority == priority)
                .count();
            let end = start + length;
            called += self.run_phase(spawner, &slots[start..end], event, &probe);
            start = end;
        }
        called
    }

    /// Runs the observers of one priority as jobs, one job each, and waits for them.
    fn run_phase<E: Sync + 'static>(
        &self,
        spawner: &Spawner,
        phase: &[Slot<E>],
        event: &E,
        probe: &Probe<'_>,
    ) -> usize {
        let called = AtomicUsize::new(0);
        spawner.parallel_chunks(0..phase.len(), 1, |range| {
            for index in range {
                let slot = &phase[index];
                let Handler::Observer(handler) = &slot.handler else {
                    continue;
                };
                if !self.inner.claim(slot) {
                    continue;
                }
                probe.call(site::<E>(slot, HandlerKind::Observer), || {
                    (**handler)(event);
                });
                called.fetch_add(1, Ordering::Relaxed);
            }
        });
        called.load(Ordering::Relaxed)
    }

    /// Like [`dispatch`](EventBus::dispatch), but delivers events of different types at the same time, as jobs on
    /// the thread pool behind `spawner`, and returns how many events it delivered when all of them are done.
    ///
    /// * **Per type, in order; across types, at the same time.** The events of the pass are grouped by type. The
    ///   events of one type are delivered one after the other in posting order, by one job; the groups run in
    ///   parallel. Two events of different types are therefore not ordered against each other, which
    ///   [`dispatch`](EventBus::dispatch) guarantees.
    /// * **The same rules as `dispatch`:** one pass over the events queued at that moment, events posted during
    ///   it wait for the next call, and a call while a pass is running (another thread, or a handler calling
    ///   `dispatch` or `dispatch_parallel`) returns 0 at once. Each event is delivered like
    ///   [`publish_mut`](EventBus::publish_mut).
    /// * **A panicking handler loses only its own event.** The other events of the pass are still delivered, the
    ///   pass waits for all jobs, and then the first panic is re-raised from this call. (`dispatch`, which can
    ///   stop at once, puts the undelivered events back instead.)
    ///
    /// Handlers of different event types now may run at the same time, so state that they share needs the usual
    /// synchronization; handlers are `Send + Sync` already, which is why this is safe.
    ///
    /// ```
    /// use std::sync::Arc;
    /// use std::sync::atomic::{AtomicU32, Ordering};
    ///
    /// use event::EventBus;
    /// use threading::{PoolConfig, ThreadPool};
    ///
    /// struct Physics(u32);
    /// struct Audio(u32);
    ///
    /// let pool = ThreadPool::new(PoolConfig::new())?;
    /// let bus = EventBus::new();
    /// let total = Arc::new(AtomicU32::new(0));
    ///
    /// let sink = Arc::clone(&total);
    /// let _physics = bus.subscribe(move |event: &Physics| {
    ///     sink.fetch_add(event.0, Ordering::SeqCst);
    /// });
    /// let sink = Arc::clone(&total);
    /// let _audio = bus.subscribe(move |event: &Audio| {
    ///     sink.fetch_add(event.0, Ordering::SeqCst);
    /// });
    ///
    /// bus.post(Physics(1));
    /// bus.post(Audio(10));
    /// bus.post(Physics(100));
    ///
    /// assert_eq!(bus.dispatch_parallel(&pool.spawner()), 3);
    /// assert_eq!(total.load(Ordering::SeqCst), 111);
    /// # Ok::<(), threading::ThreadingError>(())
    /// ```
    pub fn dispatch_parallel(&self, spawner: &Spawner) -> usize {
        let Some(mut pass) = self.inner.queue.begin_pass() else {
            return 0;
        };

        // Group the pass by event type, in order of first appearance; a group keeps the posting order.
        let mut groups: Vec<Vec<Queued>> = Vec::new();
        let mut group_of: HashMap<TypeId, usize> = HashMap::new();
        let mut delivered = 0;
        while let Some(item) = pass.pop() {
            delivered += 1;
            self.inner.count_dispatched();
            let at = *group_of.entry(item.event_type()).or_insert_with(|| {
                groups.push(Vec::new());
                groups.len() - 1
            });
            groups[at].push(item);
        }

        let first_panic: Mutex<Option<Box<dyn Any + Send>>> = Mutex::new(None);
        if !groups.is_empty() {
            spawner.parallel_chunks_mut(&mut groups, 1, |_, part: &mut [Vec<Queued>]| {
                for group in part {
                    for item in group.drain(..) {
                        let outcome = catch_unwind(AssertUnwindSafe(|| item.deliver(self)));
                        if let Err(payload) = outcome {
                            let mut slot = lock(&first_panic);
                            if slot.is_none() {
                                *slot = Some(payload);
                            }
                        }
                    }
                }
            });
        }

        let payload = first_panic
            .into_inner()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(payload) = payload {
            resume_unwind(payload);
        }
        delivered
    }

    /// How many events wait for the next [`dispatch`](EventBus::dispatch). The events of a pass that is running
    /// are not counted.
    pub fn pending(&self) -> usize {
        self.inner.queue.pending()
    }

    /// Throws away the queued events without delivering them and returns how many there were.
    pub fn clear_pending(&self) -> usize {
        let discarded = self.inner.queue.take_all();
        let count = discarded.len();
        // Dropped here, outside the queue lock: an event's `Drop` may post to this bus.
        drop(discarded);
        count
    }

    /// A handle with which other threads post events to this bus; it does not keep the bus alive.
    pub fn sender(&self) -> EventSender {
        EventSender::new(Arc::downgrade(&self.inner))
    }

    /// Switches the counters of the bus on or off (they are off by default). Turning them off keeps the values.
    ///
    /// While on, every publish and every handler call costs one relaxed atomic addition, and every post and
    /// every delivered queued event one more; with the counters off the only cost is a load per publish.
    ///
    /// ```
    /// use event::EventBus;
    ///
    /// struct Tick;
    ///
    /// let bus = EventBus::new();
    /// bus.set_stats_enabled(true);
    /// let _first = bus.subscribe(|_: &Tick| {});
    /// let _second = bus.subscribe(|_: &Tick| {});
    ///
    /// bus.publish(&Tick);
    /// bus.publish(&Tick);
    ///
    /// let stats = bus.stats();
    /// assert_eq!(stats.publishes(), 2);
    /// assert_eq!(stats.handler_calls(), 4);
    /// ```
    pub fn set_stats_enabled(&self, enabled: bool) {
        self.inner.stats.enabled.store(enabled, Ordering::Relaxed);
    }

    /// Whether the counters are running.
    pub fn stats_enabled(&self) -> bool {
        self.inner.stats.enabled.load(Ordering::Relaxed)
    }

    /// A snapshot of the counters. The counters are read one after the other, so a snapshot taken while other
    /// threads publish need not be consistent between its fields.
    pub fn stats(&self) -> BusStats {
        self.inner.stats.snapshot()
    }

    /// Sets every counter to zero; the counters keep running or stay off as they were.
    pub fn reset_stats(&self) {
        self.inner.stats.reset();
    }

    /// Lists the event types that have handlers and how many of each kind, sorted by type name. An event type
    /// whose last handler went away is not listed.
    ///
    /// ```
    /// use event::EventBus;
    ///
    /// struct Tick;
    ///
    /// let bus = EventBus::new();
    /// let _once = bus.subscribe_once(|_: &Tick| {});
    /// let _plain = bus.subscribe(|_: &Tick| {});
    ///
    /// let counts = bus.handler_counts();
    /// assert_eq!(counts.len(), 1);
    /// assert_eq!(counts[0].observers(), 2);
    /// assert_eq!(counts[0].one_shot(), 1);
    /// assert!(counts[0].name().ends_with("Tick"));
    /// ```
    pub fn handler_counts(&self) -> Vec<EventTypeInfo> {
        self.inner.event_types()
    }

    /// Calls `hook` with a [`SlowHandler`] report for every handler that runs for `threshold` or longer. Replaces
    /// the hook that was installed before.
    ///
    /// * **Every way of delivering is measured:** `publish`, `publish_mut`, `publish_parallel`, `dispatch` and
    ///   `dispatch_parallel`, observers and interceptors alike. The time is the handler's own: a handler that
    ///   publishes another event is measured with everything that event ran.
    /// * **The hook runs on the thread that ran the handler**, right after the handler returned (on a worker
    ///   thread for the parallel methods), so it must be `Send + Sync` and should be quick. It may use the bus,
    ///   including setting or clearing the hook: no lock of the bus is held while it runs. A panic in the hook
    ///   unwinds like a panic in a handler.
    /// * **A publish keeps the hook it started with.** Installing, replacing or clearing a hook affects the
    ///   publishes that start afterwards; the same holds for switching the counters on or off.
    /// * **A handler that panics is not reported.** `Duration::ZERO` reports every handler.
    /// * **Cost:** with no hook installed, nothing is measured. With one, every handler call is bracketed by two
    ///   `Instant::now()` calls, and a publish clones an `Arc`.
    ///
    /// With the `logging` feature, `SlowHandler::to_diagnostic` turns a report into a warning for the logger.
    ///
    /// ```
    /// use std::sync::{Arc, Mutex};
    /// use std::time::Duration;
    ///
    /// use event::EventBus;
    ///
    /// struct Load;
    ///
    /// let bus = EventBus::new();
    /// let reports = Arc::new(Mutex::new(Vec::new()));
    /// let sink = Arc::clone(&reports);
    /// bus.set_slow_handler_hook(Duration::from_millis(5), move |report| {
    ///     sink.lock().unwrap().push(report.elapsed());
    /// });
    ///
    /// let _subscription = bus.subscribe(|_: &Load| std::thread::sleep(Duration::from_millis(20)));
    /// bus.publish(&Load);
    /// assert_eq!(reports.lock().unwrap().len(), 1);
    /// ```
    pub fn set_slow_handler_hook<F>(&self, threshold: Duration, hook: F)
    where
        F: Fn(&SlowHandler) + Send + Sync + 'static,
    {
        let config = SlowConfig {
            threshold,
            hook: Arc::new(hook),
        };
        let previous = lock(&self.inner.slow).replace(config);
        self.inner.slow_enabled.store(true, Ordering::Release);
        // The old hook is dropped here, after the lock is released: what it captured may use the bus.
        drop(previous);
    }

    /// Removes the slow-handler hook; nothing is measured afterwards.
    pub fn clear_slow_handler_hook(&self) {
        self.inner.slow_enabled.store(false, Ordering::Release);
        let previous = lock(&self.inner.slow).take();
        // Dropped after the lock is released, like the hook that `set_slow_handler_hook` replaces.
        drop(previous);
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
            .field("pending", &self.pending())
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
