//! The queue behind `post` and `dispatch`.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::bus::{EventBus, lock};

/// A posted event, ready to be delivered: it owns the event and knows its type.
pub(crate) type Queued = Box<dyn FnOnce(&EventBus) + Send>;

/// Wraps `event` for the queue. Delivery runs the whole handler chain of the event's type, interceptors included
/// (see [`EventBus::publish_mut`]): the queue owns the event, so it can lend out `&mut`.
pub(crate) fn queued<E: Send + 'static>(event: E) -> Queued {
    Box::new(move |bus: &EventBus| {
        let mut event = event;
        bus.publish_mut(&mut event);
    })
}

/// The posted events of a bus, in the order in which they were posted.
pub(crate) struct Queue {
    items: Mutex<VecDeque<Queued>>,
    /// Set while a dispatch pass runs; only one pass runs at a time.
    dispatching: AtomicBool,
}

impl Queue {
    pub(crate) fn new() -> Self {
        Self {
            items: Mutex::new(VecDeque::new()),
            dispatching: AtomicBool::new(false),
        }
    }

    pub(crate) fn push(&self, item: Queued) {
        lock(&self.items).push_back(item);
    }

    /// How many events wait for the next pass (events of a pass in progress are not counted).
    pub(crate) fn pending(&self) -> usize {
        lock(&self.items).len()
    }

    /// Takes everything that is queued. The caller drops what it takes, outside the lock: an event's `Drop` may
    /// post to the same bus.
    pub(crate) fn take_all(&self) -> VecDeque<Queued> {
        std::mem::take(&mut *lock(&self.items))
    }

    /// Starts a dispatch pass over the events queued right now, or returns `None` if a pass is already running
    /// (on another thread, or further up this thread's stack).
    pub(crate) fn begin_pass(&self) -> Option<Pass<'_>> {
        if self
            .dispatching
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return None;
        }
        let batch = self.take_all();
        Some(Pass { queue: self, batch })
    }
}

/// One dispatch pass. Dropping it, normally or by unwinding, puts the events that were not delivered back at the
/// front of the queue and lets the next pass begin.
pub(crate) struct Pass<'a> {
    queue: &'a Queue,
    batch: VecDeque<Queued>,
}

impl Pass<'_> {
    /// The next event of this pass.
    pub(crate) fn pop(&mut self) -> Option<Queued> {
        self.batch.pop_front()
    }
}

impl Drop for Pass<'_> {
    fn drop(&mut self) {
        if !self.batch.is_empty() {
            let mut items = lock(&self.queue.items);
            while let Some(item) = self.batch.pop_back() {
                items.push_front(item);
            }
        }
        self.queue.dispatching.store(false, Ordering::Release);
    }
}
