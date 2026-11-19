//! What a handler tells the bus, and what the bus tells the publisher.

/// What an interceptor (see [`EventBus::subscribe_mut`](crate::EventBus::subscribe_mut)) tells the bus after
/// it has looked at an event.
///
/// ```
/// use event::Flow;
///
/// assert_ne!(Flow::Continue, Flow::Stop);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Flow {
    /// Hand the event on to the next handler.
    Continue,
    /// Do not call any handler after this one for this event.
    Stop,
}

/// What [`EventBus::publish_mut`](crate::EventBus::publish_mut) did.
///
/// ```
/// use event::EventBus;
///
/// struct Tick;
///
/// let bus = EventBus::new();
/// let _subscription = bus.subscribe(|_: &Tick| {});
///
/// let dispatch = bus.publish_mut(&mut Tick);
/// assert_eq!(dispatch.called(), 1);
/// assert!(!dispatch.was_stopped());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Dispatch {
    called: usize,
    stopped: bool,
}

impl Dispatch {
    pub(crate) fn new(called: usize, stopped: bool) -> Self {
        Self { called, stopped }
    }

    /// How many handlers were called, including the one that stopped the chain, if any.
    pub fn called(&self) -> usize {
        self.called
    }

    /// Whether an interceptor ended the chain by returning [`Flow::Stop`].
    pub fn was_stopped(&self) -> bool {
        self.stopped
    }
}
