//! Counters, handler counts and slow-handler reports of a bus.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::priority::EventPriority;

/// The two kinds of handler a bus knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HandlerKind {
    /// A handler that sees the event by shared reference (`subscribe`, `subscribe_once`).
    Observer,
    /// A handler that may change the event and stop the chain (`subscribe_mut`).
    Interceptor,
}

impl HandlerKind {
    /// `"observer"` or `"interceptor"`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Observer => "observer",
            Self::Interceptor => "interceptor",
        }
    }
}

impl fmt::Display for HandlerKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A handler that took longer than the threshold set with
/// [`EventBus::set_slow_handler_hook`](crate::EventBus::set_slow_handler_hook).
///
/// The report is built after the handler returned, so a handler that panics is not reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlowHandler {
    event_type: &'static str,
    handler_id: u64,
    priority: EventPriority,
    kind: HandlerKind,
    elapsed: Duration,
    threshold: Duration,
}

impl SlowHandler {
    /// The type name of the event, as `std::any::type_name` spells it (for diagnostics, not for matching).
    pub fn event_type(&self) -> &'static str {
        self.event_type
    }

    /// The id of the handler's subscription: the same number as [`Subscription::id`](crate::Subscription::id).
    pub fn handler_id(&self) -> u64 {
        self.handler_id
    }

    /// The priority the handler was subscribed with.
    pub fn priority(&self) -> EventPriority {
        self.priority
    }

    /// Whether the handler was an observer or an interceptor.
    pub fn kind(&self) -> HandlerKind {
        self.kind
    }

    /// How long the handler ran.
    pub fn elapsed(&self) -> Duration {
        self.elapsed
    }

    /// The threshold that was exceeded.
    pub fn threshold(&self) -> Duration {
        self.threshold
    }
}

#[cfg(feature = "logging")]
impl SlowHandler {
    /// Turns the report into a warning for the engine's logging core. Available with the `logging` feature.
    ///
    /// The code is `CV-EVENT-SLOW-0001` and the module is `event`; the event type, the handler id, the kind, the
    /// priority and the times (in microseconds) go into the extra fields. Hand the result to a logger:
    ///
    /// ```ignore
    /// bus.set_slow_handler_hook(Duration::from_millis(2), move |slow| {
    ///     logger.emit(slow.to_diagnostic());
    /// });
    /// ```
    pub fn to_diagnostic(&self) -> log_core::Diagnostic {
        let message = format!(
            "{} #{} of {} took {:?} (threshold {:?})",
            self.kind, self.handler_id, self.event_type, self.elapsed, self.threshold
        );
        log_core::DiagnosticBuilder::new(
            "CV-EVENT",
            "SLOW",
            1,
            log_core::Severity::Warning,
            message,
        )
        .module("event")
        .extra("event_type", self.event_type)
        .extra("handler_id", self.handler_id.to_string())
        .extra("kind", self.kind.as_str())
        .extra("priority", format!("{:?}", self.priority))
        .extra("elapsed_us", self.elapsed.as_micros().to_string())
        .extra("threshold_us", self.threshold.as_micros().to_string())
        .build()
    }
}

/// How many handlers of each kind are subscribed to one event type; see
/// [`EventBus::handler_counts`](crate::EventBus::handler_counts).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventTypeInfo {
    name: &'static str,
    observers: usize,
    interceptors: usize,
    one_shot: usize,
}

impl EventTypeInfo {
    pub(crate) fn new(
        name: &'static str,
        observers: usize,
        interceptors: usize,
        one_shot: usize,
    ) -> Self {
        Self {
            name,
            observers,
            interceptors,
            one_shot,
        }
    }

    /// The type name of the event, as `std::any::type_name` spells it (for diagnostics, not for matching).
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// How many observers are subscribed, one-shot observers included.
    pub fn observers(&self) -> usize {
        self.observers
    }

    /// How many interceptors are subscribed.
    pub fn interceptors(&self) -> usize {
        self.interceptors
    }

    /// How many of the observers are one-shot handlers that have not run yet.
    pub fn one_shot(&self) -> usize {
        self.one_shot
    }

    /// All handlers of the type: observers plus interceptors.
    pub fn total(&self) -> usize {
        self.observers + self.interceptors
    }
}

/// A snapshot of the counters of a bus; see [`EventBus::stats`](crate::EventBus::stats).
///
/// The counters only run while collection is switched on with
/// [`EventBus::set_stats_enabled`](crate::EventBus::set_stats_enabled).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BusStats {
    publishes: u64,
    handler_calls: u64,
    posted: u64,
    dispatched: u64,
    max_pending: u64,
    slow_handlers: u64,
}

impl BusStats {
    /// How many times an event was published: `publish`, `publish_mut`, `publish_parallel`, and every queued
    /// event the bus delivered (which goes through `publish_mut`). A publish with no subscribers counts.
    pub fn publishes(&self) -> u64 {
        self.publishes
    }

    /// How many handlers ran.
    pub fn handler_calls(&self) -> u64 {
        self.handler_calls
    }

    /// How many events were posted to the queue, by the bus or by an `EventSender`.
    pub fn posted(&self) -> u64 {
        self.posted
    }

    /// How many queued events `dispatch` and `dispatch_parallel` delivered.
    pub fn dispatched(&self) -> u64 {
        self.dispatched
    }

    /// The most events that were waiting in the queue right after a post.
    pub fn max_pending(&self) -> u64 {
        self.max_pending
    }

    /// How many slow-handler reports were produced.
    pub fn slow_handlers(&self) -> u64 {
        self.slow_handlers
    }
}

/// The live counters behind [`BusStats`].
#[derive(Default)]
pub(crate) struct Stats {
    pub(crate) enabled: AtomicBool,
    pub(crate) publishes: AtomicU64,
    pub(crate) handler_calls: AtomicU64,
    pub(crate) posted: AtomicU64,
    pub(crate) dispatched: AtomicU64,
    pub(crate) max_pending: AtomicU64,
    pub(crate) slow_handlers: AtomicU64,
}

impl Stats {
    pub(crate) fn snapshot(&self) -> BusStats {
        BusStats {
            publishes: self.publishes.load(Ordering::Relaxed),
            handler_calls: self.handler_calls.load(Ordering::Relaxed),
            posted: self.posted.load(Ordering::Relaxed),
            dispatched: self.dispatched.load(Ordering::Relaxed),
            max_pending: self.max_pending.load(Ordering::Relaxed),
            slow_handlers: self.slow_handlers.load(Ordering::Relaxed),
        }
    }

    pub(crate) fn reset(&self) {
        self.publishes.store(0, Ordering::Relaxed);
        self.handler_calls.store(0, Ordering::Relaxed);
        self.posted.store(0, Ordering::Relaxed);
        self.dispatched.store(0, Ordering::Relaxed);
        self.max_pending.store(0, Ordering::Relaxed);
        self.slow_handlers.store(0, Ordering::Relaxed);
    }
}

/// The function the bus calls with a slow-handler report.
pub(crate) type SlowHook = Arc<dyn Fn(&SlowHandler) + Send + Sync>;

/// The threshold and the hook that were installed together.
#[derive(Clone)]
pub(crate) struct SlowConfig {
    pub(crate) threshold: Duration,
    pub(crate) hook: SlowHook,
}

/// Where a handler call happens, for the counters and the reports.
#[derive(Clone, Copy)]
pub(crate) struct Site {
    pub(crate) event_type: &'static str,
    pub(crate) id: u64,
    pub(crate) priority: EventPriority,
    pub(crate) kind: HandlerKind,
}

/// What one publish needs to know about the diagnostics, read once when it starts. With both parts switched
/// off (the default) a handler call costs two atomic loads more than a plain call, taken at the start.
pub(crate) struct Probe<'a> {
    stats: Option<&'a Stats>,
    slow: Option<SlowConfig>,
}

impl<'a> Probe<'a> {
    pub(crate) fn new(stats: Option<&'a Stats>, slow: Option<SlowConfig>) -> Self {
        Self { stats, slow }
    }

    /// Counts one publish.
    pub(crate) fn publish_started(&self) {
        if let Some(stats) = self.stats {
            stats.publishes.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Runs one handler: counts the call and reports the handler if it was slow. A handler that unwinds is
    /// counted but not reported.
    #[inline]
    pub(crate) fn call<R>(&self, site: Site, run: impl FnOnce() -> R) -> R {
        if let Some(stats) = self.stats {
            stats.handler_calls.fetch_add(1, Ordering::Relaxed);
        }
        let Some(config) = &self.slow else {
            return run();
        };
        let started = Instant::now();
        let result = run();
        let elapsed = started.elapsed();
        if elapsed >= config.threshold {
            self.report(config, site, elapsed);
        }
        result
    }

    fn report(&self, config: &SlowConfig, site: Site, elapsed: Duration) {
        if let Some(stats) = self.stats {
            stats.slow_handlers.fetch_add(1, Ordering::Relaxed);
        }
        (config.hook)(&SlowHandler {
            event_type: site.event_type,
            handler_id: site.id,
            priority: site.priority,
            kind: site.kind,
            elapsed,
            threshold: config.threshold,
        });
    }
}
