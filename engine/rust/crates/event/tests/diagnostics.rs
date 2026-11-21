//! Counters, handler counts and slow-handler reports (E5).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use event::{
    BusStats, EventBus, EventPriority, EventTypeInfo, Flow, HandlerKind, SlowHandler, Subscription,
};
use threading::{PoolConfig, ThreadPool, WorkerCount};

struct Ping;
struct Pong;

fn make_pool(workers: usize) -> ThreadPool {
    ThreadPool::new(PoolConfig::new().with_workers(WorkerCount::Exact(workers))).unwrap()
}

/// Subscribes `count` observers of `Ping` that do nothing.
fn idle_observers(bus: &EventBus, count: usize) -> Vec<Subscription> {
    (0..count).map(|_| bus.subscribe(|_: &Ping| {})).collect()
}

/// Installs a hook that stores every report in the returned list.
fn collect_reports(bus: &EventBus, threshold: Duration) -> Arc<Mutex<Vec<SlowHandler>>> {
    let reports = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&reports);
    bus.set_slow_handler_hook(threshold, move |report: &SlowHandler| {
        sink.lock().unwrap().push(report.clone());
    });
    reports
}

fn sleepy(millis: u64) -> impl Fn(&Ping) + Send + Sync + 'static {
    move |_: &Ping| thread::sleep(Duration::from_millis(millis))
}

fn find<'a>(infos: &'a [EventTypeInfo], suffix: &str) -> &'a EventTypeInfo {
    infos
        .iter()
        .find(|info| info.name().ends_with(suffix))
        .unwrap_or_else(|| panic!("no event type that ends with {suffix}"))
}

// ----- counters ---------------------------------------------------------------------------------

#[test]
fn the_counters_are_off_by_default_and_count_nothing() {
    let bus = EventBus::new();
    let _subscriptions = idle_observers(&bus, 2);
    assert!(!bus.stats_enabled());

    bus.publish(&Ping);
    bus.post(Ping);
    bus.dispatch();
    assert_eq!(bus.stats(), BusStats::default());
}

#[test]
fn publishes_and_handler_calls_are_counted() {
    let bus = EventBus::new();
    bus.set_stats_enabled(true);
    let _subscriptions = idle_observers(&bus, 2);

    bus.publish(&Ping);
    bus.publish(&Ping);
    bus.publish(&Ping);
    assert_eq!(bus.stats().publishes(), 3);
    assert_eq!(bus.stats().handler_calls(), 6);

    bus.publish(&Pong);
    assert_eq!(
        bus.stats().publishes(),
        4,
        "a publish nobody listens to counts too"
    );
    assert_eq!(bus.stats().handler_calls(), 6);
}

#[test]
fn publish_mut_and_publish_parallel_are_counted_too() {
    let pool = make_pool(2);
    let bus = EventBus::new();
    bus.set_stats_enabled(true);
    let _observer = bus.subscribe(|_: &Ping| {});
    let _interceptor = bus.subscribe_mut(|_: &mut Ping| Flow::Continue);

    bus.publish_mut(&mut Ping);
    assert_eq!(bus.stats().publishes(), 1);
    assert_eq!(bus.stats().handler_calls(), 2);

    bus.publish_parallel(&pool.spawner(), &Ping);
    assert_eq!(bus.stats().publishes(), 2);
    assert_eq!(
        bus.stats().handler_calls(),
        3,
        "only the observer runs in a parallel publish"
    );
}

#[test]
fn the_queue_counters_follow_posts_and_dispatches() {
    let bus = EventBus::new();
    bus.set_stats_enabled(true);
    let _subscriptions = idle_observers(&bus, 1);
    let sender = bus.sender();

    bus.post(Ping);
    bus.post(Ping);
    sender.post(Ping).unwrap();
    assert_eq!(bus.stats().posted(), 3);
    assert_eq!(bus.stats().max_pending(), 3);

    assert_eq!(bus.dispatch(), 3);
    assert_eq!(bus.stats().dispatched(), 3);
    assert_eq!(
        bus.stats().publishes(),
        3,
        "queued events are delivered through publish_mut"
    );
    assert_eq!(bus.stats().handler_calls(), 3);

    bus.post(Ping);
    assert_eq!(bus.stats().posted(), 4);
    assert_eq!(
        bus.stats().max_pending(),
        3,
        "the longest queue stays the longest"
    );
}

#[test]
fn dispatch_parallel_counts_dispatched_events() {
    let pool = make_pool(2);
    let bus = EventBus::new();
    bus.set_stats_enabled(true);
    let _ping = bus.subscribe(|_: &Ping| {});
    let _pong = bus.subscribe(|_: &Pong| {});

    bus.post(Ping);
    bus.post(Pong);
    bus.post(Ping);
    assert_eq!(bus.dispatch_parallel(&pool.spawner()), 3);
    assert_eq!(bus.stats().dispatched(), 3);
    assert_eq!(bus.stats().handler_calls(), 3);
}

#[test]
fn reset_zeroes_the_counters_and_keeps_collecting() {
    let bus = EventBus::new();
    bus.set_stats_enabled(true);
    let _subscriptions = idle_observers(&bus, 1);

    bus.publish(&Ping);
    assert_eq!(bus.stats().publishes(), 1);

    bus.reset_stats();
    assert_eq!(bus.stats(), BusStats::default());
    assert!(bus.stats_enabled());

    bus.publish(&Ping);
    assert_eq!(bus.stats().publishes(), 1);
}

#[test]
fn switching_the_counters_off_keeps_the_values_and_stops_counting() {
    let bus = EventBus::new();
    bus.set_stats_enabled(true);
    let _subscriptions = idle_observers(&bus, 1);
    bus.publish(&Ping);

    bus.set_stats_enabled(false);
    bus.publish(&Ping);
    assert_eq!(bus.stats().publishes(), 1);
    assert_eq!(bus.stats().handler_calls(), 1);
}

#[test]
fn clones_of_a_bus_share_the_counters() {
    let bus = EventBus::new();
    let clone = bus.clone();
    bus.set_stats_enabled(true);
    let _subscriptions = idle_observers(&bus, 1);

    clone.publish(&Ping);
    assert_eq!(bus.stats().publishes(), 1);
}

// ----- handler counts ---------------------------------------------------------------------------

#[test]
fn an_empty_bus_lists_no_event_types() {
    assert!(EventBus::new().handler_counts().is_empty());
}

#[test]
fn handler_counts_describe_each_event_type() {
    let bus = EventBus::new();
    let _plain = bus.subscribe(|_: &Ping| {});
    let once = bus.subscribe_once(|_: &Ping| {});
    let _interceptor = bus.subscribe_mut(|_: &mut Ping| Flow::Continue);
    let _pong = bus.subscribe(|_: &Pong| {});

    let counts = bus.handler_counts();
    assert_eq!(counts.len(), 2);
    let names: Vec<&str> = counts.iter().map(EventTypeInfo::name).collect();
    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(names, sorted, "sorted by name");

    let ping = find(&counts, "Ping");
    assert_eq!(ping.observers(), 2);
    assert_eq!(ping.interceptors(), 1);
    assert_eq!(ping.one_shot(), 1);
    assert_eq!(ping.total(), 3);

    let pong = find(&counts, "Pong");
    assert_eq!(
        (pong.observers(), pong.interceptors(), pong.one_shot()),
        (1, 0, 0)
    );

    drop(once);
    let counts = bus.handler_counts();
    let ping = find(&counts, "Ping");
    assert_eq!((ping.observers(), ping.one_shot()), (1, 0));
}

#[test]
fn a_one_shot_handler_that_has_run_is_no_longer_counted() {
    let bus = EventBus::new();
    let _once = bus.subscribe_once(|_: &Ping| {});
    assert_eq!(find(&bus.handler_counts(), "Ping").one_shot(), 1);

    bus.publish(&Ping);
    assert!(bus.handler_counts().is_empty());
}

#[test]
fn an_event_type_without_handlers_is_not_listed() {
    let bus = EventBus::new();
    let subscription = bus.subscribe(|_: &Ping| {});
    assert_eq!(bus.handler_counts().len(), 1);

    drop(subscription);
    assert!(bus.handler_counts().is_empty());
}

// ----- slow handlers ----------------------------------------------------------------------------

#[test]
fn a_slow_handler_is_reported_with_its_details() {
    let bus = EventBus::new();
    let reports = collect_reports(&bus, Duration::from_millis(10));
    let subscription = bus.subscribe_with_priority(EventPriority::High, sleepy(30));

    bus.publish(&Ping);

    let reports = reports.lock().unwrap();
    assert_eq!(reports.len(), 1);
    let report = &reports[0];
    assert!(report.event_type().ends_with("Ping"));
    assert_eq!(report.handler_id(), subscription.id());
    assert_eq!(report.priority(), EventPriority::High);
    assert_eq!(report.kind(), HandlerKind::Observer);
    assert_eq!(report.threshold(), Duration::from_millis(10));
    assert!(report.elapsed() >= Duration::from_millis(30));
}

#[test]
fn fast_handlers_are_not_reported() {
    let bus = EventBus::new();
    let reports = collect_reports(&bus, Duration::from_secs(3600));
    let _subscriptions = idle_observers(&bus, 3);

    bus.publish(&Ping);
    assert!(reports.lock().unwrap().is_empty());
}

#[test]
fn a_zero_threshold_reports_every_handler() {
    let bus = EventBus::new();
    let reports = collect_reports(&bus, Duration::ZERO);
    let _subscriptions = idle_observers(&bus, 3);

    bus.publish(&Ping);
    assert_eq!(reports.lock().unwrap().len(), 3);
}

#[test]
fn an_interceptor_is_reported_as_an_interceptor() {
    let bus = EventBus::new();
    let reports = collect_reports(&bus, Duration::from_millis(5));
    let _interceptor = bus.subscribe_mut(|_: &mut Ping| {
        thread::sleep(Duration::from_millis(20));
        Flow::Continue
    });

    bus.publish_mut(&mut Ping);
    let reports = reports.lock().unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].kind(), HandlerKind::Interceptor);
}

#[test]
fn queued_events_are_measured_too() {
    let bus = EventBus::new();
    let reports = collect_reports(&bus, Duration::from_millis(5));
    let _subscription = bus.subscribe(sleepy(20));

    bus.post(Ping);
    assert!(
        reports.lock().unwrap().is_empty(),
        "nothing runs before dispatch"
    );
    bus.dispatch();
    assert_eq!(reports.lock().unwrap().len(), 1);
}

#[test]
fn parallel_deliveries_are_measured_on_their_worker_threads() {
    let pool = make_pool(2);
    let spawner = pool.spawner();
    let bus = EventBus::new();
    let reports = collect_reports(&bus, Duration::from_millis(5));
    let _first = bus.subscribe(sleepy(20));
    let _second = bus.subscribe(sleepy(20));

    bus.publish_parallel(&spawner, &Ping);
    assert_eq!(reports.lock().unwrap().len(), 2);

    reports.lock().unwrap().clear();
    let _pong = bus.subscribe(|_: &Pong| thread::sleep(Duration::from_millis(20)));
    bus.post(Pong);
    bus.post(Ping);
    bus.dispatch_parallel(&spawner);
    assert_eq!(
        reports.lock().unwrap().len(),
        3,
        "two Ping handlers and the Pong handler"
    );
}

#[test]
fn clearing_the_hook_stops_the_reports() {
    let bus = EventBus::new();
    let reports = collect_reports(&bus, Duration::from_millis(5));
    let _subscription = bus.subscribe(sleepy(20));

    bus.publish(&Ping);
    assert_eq!(reports.lock().unwrap().len(), 1);

    bus.clear_slow_handler_hook();
    bus.publish(&Ping);
    assert_eq!(reports.lock().unwrap().len(), 1);
}

#[test]
fn a_new_hook_replaces_the_old_one() {
    let bus = EventBus::new();
    let old = collect_reports(&bus, Duration::from_millis(5));
    let new = collect_reports(&bus, Duration::from_millis(5));
    let _subscription = bus.subscribe(sleepy(20));

    bus.publish(&Ping);
    assert!(old.lock().unwrap().is_empty());
    assert_eq!(new.lock().unwrap().len(), 1);
}

#[test]
fn slow_handlers_are_counted_when_the_counters_run() {
    let bus = EventBus::new();
    bus.set_stats_enabled(true);
    let _reports = collect_reports(&bus, Duration::from_millis(5));
    let _slow = bus.subscribe(sleepy(20));
    let _fast = bus.subscribe(|_: &Ping| {});

    bus.publish(&Ping);
    assert_eq!(bus.stats().handler_calls(), 2);
    assert_eq!(bus.stats().slow_handlers(), 1);
}

#[test]
fn a_hook_may_reconfigure_the_bus() {
    let bus = EventBus::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let weak = bus.downgrade();
    bus.set_slow_handler_hook(Duration::ZERO, move |_| {
        counter.fetch_add(1, Ordering::SeqCst);
        if let Some(bus) = weak.upgrade() {
            bus.clear_slow_handler_hook();
        }
    });
    let _subscriptions = idle_observers(&bus, 2);

    bus.publish(&Ping);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "a publish keeps the hook it started with, although the first report cleared it"
    );

    bus.publish(&Ping);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "the next publish has no hook"
    );
}
