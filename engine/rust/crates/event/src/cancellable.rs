//! An event with a veto.

use std::ops::{Deref, DerefMut};

/// An event that handlers may cancel.
///
/// Cancelling is an ordinary piece of state, not a rule of the bus: [`cancel`](Cancellable::cancel) sets a flag,
/// handlers that run later can look at it with [`is_cancelled`](Cancellable::is_cancelled), and the publisher
/// decides what a cancelled event means (usually: do not carry out the action the event announced). It does not
/// stop the chain by itself; for that an interceptor returns [`Flow::Stop`](crate::Flow::Stop).
///
/// `Cancellable<E>` dereferences to `E`, so handlers read and change the wrapped event directly. The event type
/// that handlers subscribe to is `Cancellable<E>`, not `E`.
///
/// ```
/// use event::{Cancellable, EventBus, Flow};
///
/// struct Move {
///     to: (i32, i32),
/// }
///
/// let bus = EventBus::new();
/// let _wall = bus.subscribe_mut(|attempt: &mut Cancellable<Move>| {
///     if attempt.to.0 < 0 {
///         attempt.cancel();
///     }
///     Flow::Continue
/// });
///
/// let mut attempt = Cancellable::new(Move { to: (-1, 0) });
/// bus.publish_mut(&mut attempt);
/// assert!(attempt.is_cancelled());
///
/// let mut attempt = Cancellable::new(Move { to: (3, 0) });
/// bus.publish_mut(&mut attempt);
/// assert!(!attempt.is_cancelled());
/// ```
#[derive(Debug, Clone, Default)]
pub struct Cancellable<E> {
    event: E,
    cancelled: bool,
}

impl<E> Cancellable<E> {
    /// Wraps `event`; it starts out not cancelled.
    pub fn new(event: E) -> Self {
        Self {
            event,
            cancelled: false,
        }
    }

    /// Cancels the event. Cancelling twice is the same as cancelling once.
    pub fn cancel(&mut self) {
        self.cancelled = true;
    }

    /// Whether some handler has cancelled the event.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled
    }

    /// The wrapped event, without the cancellation state.
    pub fn into_inner(self) -> E {
        self.event
    }
}

impl<E> Deref for Cancellable<E> {
    type Target = E;

    fn deref(&self) -> &E {
        &self.event
    }
}

impl<E> DerefMut for Cancellable<E> {
    fn deref_mut(&mut self) -> &mut E {
        &mut self.event
    }
}
