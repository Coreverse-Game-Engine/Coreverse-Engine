//! A handle for posting events from other threads.

use std::error::Error;
use std::fmt;
use std::sync::Weak;

use crate::bus::Inner;
use crate::queue::queued;

/// A cloneable handle that posts events to an [`EventBus`](crate::EventBus) without keeping it alive.
///
/// This is the producer end of the bus's queue: give a clone to every worker thread (or any other code that must
/// not own the bus) and let it [`post`](EventSender::post) events; the thread that owns the bus delivers them
/// with [`EventBus::dispatch`](crate::EventBus::dispatch). Once the bus is gone, `post` hands the event back in a
/// [`PostError`], which is also how a worker finds out that it should stop.
///
/// ```
/// use std::sync::Arc;
/// use std::sync::atomic::{AtomicU32, Ordering};
/// use std::thread;
///
/// use event::EventBus;
///
/// struct Finished(u32);
///
/// let bus = EventBus::new();
/// let total = Arc::new(AtomicU32::new(0));
/// let sink = Arc::clone(&total);
/// let _subscription = bus.subscribe(move |finished: &Finished| {
///     sink.fetch_add(finished.0, Ordering::SeqCst);
/// });
///
/// let sender = bus.sender();
/// let worker = thread::spawn(move || {
///     for number in 1..=3 {
///         sender.post(Finished(number)).unwrap();
///     }
/// });
/// worker.join().unwrap();
///
/// assert_eq!(bus.dispatch(), 3);
/// assert_eq!(total.load(Ordering::SeqCst), 6);
/// ```
#[derive(Clone)]
pub struct EventSender {
    inner: Weak<Inner>,
}

impl EventSender {
    pub(crate) fn new(inner: Weak<Inner>) -> Self {
        Self { inner }
    }

    /// Queues `event` for the next [`dispatch`](crate::EventBus::dispatch) of the bus.
    ///
    /// Returns the event inside a [`PostError`] if the bus no longer exists.
    pub fn post<E: Send + 'static>(&self, event: E) -> Result<(), PostError<E>> {
        match self.inner.upgrade() {
            Some(inner) => {
                inner.queue.push(queued(event));
                Ok(())
            }
            None => Err(PostError { event }),
        }
    }

    /// Whether the bus still exists.
    pub fn is_connected(&self) -> bool {
        self.inner.strong_count() > 0
    }
}

impl fmt::Debug for EventSender {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EventSender")
            .field("connected", &self.is_connected())
            .finish_non_exhaustive()
    }
}

/// The error of [`EventSender::post`]: the bus is gone. It carries the event that could not be posted.
///
/// ```
/// use event::EventBus;
///
/// let bus = EventBus::new();
/// let sender = bus.sender();
/// drop(bus);
///
/// let error = sender.post(5_u32).unwrap_err();
/// assert_eq!(error.into_inner(), 5);
/// ```
pub struct PostError<E> {
    event: E,
}

impl<E> PostError<E> {
    /// The event that was not posted.
    pub fn into_inner(self) -> E {
        self.event
    }
}

impl<E> fmt::Debug for PostError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PostError").finish_non_exhaustive()
    }
}

impl<E> fmt::Display for PostError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the event bus no longer exists")
    }
}

impl<E> Error for PostError<E> {}
