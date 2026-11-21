//! The handle that keeps a handler subscribed.

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};

use crate::bus::Inner;

/// A handler's registration on an [`EventBus`](crate::EventBus).
///
/// **Dropping a `Subscription` unsubscribes the handler.** That is what makes subscribing safe to use from
/// code that has a lifetime of its own (a system, a window, a scene): when the owner goes away, its handlers
/// go with it. To keep a handler for as long as the bus lives, call [`Subscription::detach`].
///
/// A `Subscription` does not keep the bus alive; when the last [`EventBus`](crate::EventBus) is dropped, the
/// handlers are dropped with it and every `Subscription` becomes inactive.
///
/// ```
/// use event::EventBus;
///
/// struct Ping;
///
/// let bus = EventBus::new();
/// let subscription = bus.subscribe(|_: &Ping| {});
/// assert_eq!(bus.subscriber_count::<Ping>(), 1);
///
/// drop(subscription);
/// assert_eq!(bus.subscriber_count::<Ping>(), 0);
/// ```
#[must_use = "dropping a Subscription unsubscribes its handler; call `detach()` to keep it"]
pub struct Subscription {
    bus: Weak<Inner>,
    id: u64,
    active: Arc<AtomicBool>,
    remove: fn(&Inner, u64) -> bool,
    armed: bool,
}

impl Subscription {
    pub(crate) fn new(
        bus: Weak<Inner>,
        id: u64,
        active: Arc<AtomicBool>,
        remove: fn(&Inner, u64) -> bool,
    ) -> Self {
        Self {
            bus,
            id,
            active,
            remove,
            armed: true,
        }
    }

    /// A number that identifies this subscription among the others of the same bus (never reused).
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Whether the handler is still subscribed: `false` once it has been unsubscribed or once the bus is gone.
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire) && self.bus.strong_count() > 0
    }

    /// Unsubscribes the handler now. Returns `true` if the handler was still subscribed and `false` if it was
    /// already gone (the bus was dropped).
    ///
    /// If a [`publish`](crate::EventBus::publish) is running at this moment, the handler is skipped by it
    /// unless it has already started.
    pub fn unsubscribe(mut self) -> bool {
        self.armed = false;
        self.release()
    }

    /// Gives up this handle without unsubscribing: the handler stays subscribed for as long as the bus lives
    /// and can no longer be removed individually.
    pub fn detach(mut self) {
        self.armed = false;
    }

    fn release(&self) -> bool {
        match self.bus.upgrade() {
            Some(inner) => (self.remove)(&inner, self.id),
            None => {
                self.active.store(false, Ordering::Release);
                false
            }
        }
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        if self.armed {
            self.release();
        }
    }
}

impl fmt::Debug for Subscription {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Subscription")
            .field("id", &self.id)
            .field("active", &self.is_active())
            .finish_non_exhaustive()
    }
}
