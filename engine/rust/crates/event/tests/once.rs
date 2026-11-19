//! One-shot handlers (E2).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use event::{EventBus, EventPriority, Flow, Subscription};

struct Ping;

type Log = Arc<Mutex<Vec<&'static str>>>;

fn new_log() -> Log {
    Arc::new(Mutex::new(Vec::new()))
}

fn entries(log: &Log) -> Vec<&'static str> {
    log.lock().unwrap().clone()
}

fn log_on(bus: &EventBus, priority: EventPriority, log: &Log, what: &'static str) -> Subscription {
    let log = Arc::clone(log);
    bus.subscribe_with_priority(priority, move |_: &Ping| log.lock().unwrap().push(what))
}

fn log_once(
    bus: &EventBus,
    priority: EventPriority,
    log: &Log,
    what: &'static str,
) -> Subscription {
    let log = Arc::clone(log);
    bus.subscribe_once_with_priority(priority, move |_: &Ping| log.lock().unwrap().push(what))
}

#[test]
fn a_once_handler_runs_once_and_is_removed() {
    let bus = EventBus::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let _once = bus.subscribe_once(move |_: &Ping| {
        counter.fetch_add(1, Ordering::SeqCst);
    });
    assert_eq!(bus.subscriber_count::<Ping>(), 1);

    assert_eq!(bus.publish(&Ping), 1);
    assert_eq!(bus.publish(&Ping), 0);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(bus.subscriber_count::<Ping>(), 0);
}

#[test]
fn the_subscription_is_inactive_once_the_handler_has_run() {
    let bus = EventBus::new();
    let once = bus.subscribe_once(|_: &Ping| {});
    assert!(once.is_active());

    bus.publish(&Ping);
    assert!(!once.is_active());
    assert!(!once.unsubscribe(), "there was nothing left to remove");
}

#[test]
fn dropping_the_subscription_first_cancels_the_handler() {
    let bus = EventBus::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let once = bus.subscribe_once(move |_: &Ping| {
        counter.fetch_add(1, Ordering::SeqCst);
    });

    drop(once);
    assert_eq!(bus.publish(&Ping), 0);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn a_once_handler_may_consume_what_it_captured() {
    let bus = EventBus::new();
    let (sender, receiver) = mpsc::channel();
    let owned = String::from("moved");
    let _once = bus.subscribe_once(move |_: &Ping| {
        sender.send(owned).unwrap();
    });

    bus.publish(&Ping);
    assert_eq!(receiver.try_recv().unwrap(), "moved");
}

#[test]
fn once_handlers_follow_priority_order_like_any_other() {
    let bus = EventBus::new();
    let log = new_log();
    let _low = log_once(&bus, EventPriority::Low, &log, "low");
    let _normal = log_on(&bus, EventPriority::Normal, &log, "normal");
    let _high = log_once(&bus, EventPriority::High, &log, "high");

    assert_eq!(bus.publish(&Ping), 3);
    assert_eq!(entries(&log), ["high", "normal", "low"]);

    assert_eq!(bus.publish(&Ping), 1);
    assert_eq!(entries(&log), ["high", "normal", "low", "normal"]);
}

#[test]
fn a_once_handler_is_not_run_again_by_the_event_it_publishes() {
    let bus = EventBus::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let weak = bus.downgrade();
    let counter = Arc::clone(&calls);
    let _once = bus.subscribe_once(move |_: &Ping| {
        counter.fetch_add(1, Ordering::SeqCst);
        if let Some(bus) = weak.upgrade() {
            bus.publish(&Ping);
        }
    });

    assert_eq!(bus.publish(&Ping), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn threads_publishing_together_run_a_once_handler_exactly_once() {
    let bus = EventBus::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let _once = bus.subscribe_once(move |_: &Ping| {
        counter.fetch_add(1, Ordering::SeqCst);
    });

    thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| {
                for _ in 0..500 {
                    bus.publish(&Ping);
                }
            });
        }
    });

    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(bus.subscriber_count::<Ping>(), 0);
}

#[test]
fn once_observers_take_part_in_publish_mut_too() {
    let bus = EventBus::new();
    let _once = bus.subscribe_once(|_: &Ping| {});
    let _interceptor = bus.subscribe_mut(|_: &mut Ping| Flow::Continue);

    assert_eq!(bus.publish_mut(&mut Ping).called(), 2);
    assert_eq!(bus.publish_mut(&mut Ping).called(), 1);
}

#[test]
fn a_once_handler_that_panics_is_gone_afterwards() {
    use std::panic::{AssertUnwindSafe, catch_unwind};

    let bus = EventBus::new();
    let _once = bus.subscribe_once(|_: &Ping| panic!("boom"));

    let result = catch_unwind(AssertUnwindSafe(|| bus.publish(&Ping)));
    assert!(result.is_err());
    assert_eq!(bus.publish(&Ping), 0);
    assert_eq!(bus.subscriber_count::<Ping>(), 0);
}
