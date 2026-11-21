//! Behaviour of the synchronous event bus (E1).

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use event::{EventBus, EventPriority, Subscription, WeakEventBus};

struct Ping;
struct Pong;
struct Number(u32);
struct Countdown(u32);

type Log = Arc<Mutex<Vec<&'static str>>>;

fn new_log() -> Log {
    Arc::new(Mutex::new(Vec::new()))
}

fn entries(log: &Log) -> Vec<&'static str> {
    log.lock().unwrap().clone()
}

/// Subscribes a handler that appends `what` to `log` each time an `E` is published.
fn log_on<E: 'static>(
    bus: &EventBus,
    priority: EventPriority,
    log: &Log,
    what: &'static str,
) -> Subscription {
    let log = Arc::clone(log);
    bus.subscribe_with_priority(priority, move |_: &E| log.lock().unwrap().push(what))
}

/// How many repetitions a stress test runs: the full number natively, a few under Miri, which is orders of
/// magnitude slower and only needs to see every code path.
fn scaled(native: u32, under_miri: u32) -> u32 {
    if cfg!(miri) { under_miri } else { native }
}

/// Runs `work` on its own thread and fails the test if it has not finished in time (a deadlock): after 20 seconds,
/// or after 15 minutes under Miri.
fn within_patience<T: Send + 'static>(what: &str, work: impl FnOnce() -> T + Send + 'static) -> T {
    let patience = Duration::from_secs(if cfg!(miri) { 900 } else { 20 });
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let _ = sender.send(work());
    });
    receiver
        .recv_timeout(patience)
        .unwrap_or_else(|_| panic!("{what} did not finish in time (deadlock or panic)"))
}

#[test]
fn publishing_without_subscribers_calls_nobody() {
    let bus = EventBus::new();
    assert_eq!(bus.publish(&Ping), 0);
}

#[test]
fn a_handler_receives_the_event() {
    let bus = EventBus::new();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let _subscription = bus.subscribe(move |number: &Number| sink.lock().unwrap().push(number.0));

    assert_eq!(bus.publish(&Number(5)), 1);
    assert_eq!(bus.publish(&Number(8)), 1);
    assert_eq!(*seen.lock().unwrap(), [5, 8]);
}

#[test]
fn handlers_only_get_their_own_event_type() {
    let bus = EventBus::new();
    let log = new_log();
    let _ping = log_on::<Ping>(&bus, EventPriority::Normal, &log, "ping");
    let _pong = log_on::<Pong>(&bus, EventPriority::Normal, &log, "pong");

    assert_eq!(bus.publish(&Pong), 1);
    assert_eq!(entries(&log), ["pong"]);
}

#[test]
fn an_event_may_be_any_static_type() {
    let bus = EventBus::new();
    let calls = Arc::new(AtomicUsize::new(0));

    let counter = Arc::clone(&calls);
    let _string = bus.subscribe(move |_: &String| {
        counter.fetch_add(1, Ordering::SeqCst);
    });
    let counter = Arc::clone(&calls);
    let _tuple = bus.subscribe(move |_: &(u8, u8)| {
        counter.fetch_add(1, Ordering::SeqCst);
    });
    let counter = Arc::clone(&calls);
    let _text = bus.subscribe(move |_: &&'static str| {
        counter.fetch_add(1, Ordering::SeqCst);
    });
    let counter = Arc::clone(&calls);
    let _list = bus.subscribe(move |_: &Vec<i32>| {
        counter.fetch_add(1, Ordering::SeqCst);
    });

    bus.publish(&String::from("a"));
    bus.publish(&(1_u8, 2_u8));
    bus.publish(&"b");
    let list = Vec::from([1, 2, 3]);
    bus.publish(&list);
    assert_eq!(calls.load(Ordering::SeqCst), 4);
}

#[test]
fn priorities_order_handlers_from_high_to_low() {
    let bus = EventBus::new();
    let log = new_log();
    let _low = log_on::<Ping>(&bus, EventPriority::Low, &log, "low");
    let _normal_a = log_on::<Ping>(&bus, EventPriority::Normal, &log, "normal-a");
    let _high = log_on::<Ping>(&bus, EventPriority::High, &log, "high");
    let _normal_b = log_on::<Ping>(&bus, EventPriority::Normal, &log, "normal-b");

    assert_eq!(bus.publish(&Ping), 4);
    assert_eq!(entries(&log), ["high", "normal-a", "normal-b", "low"]);
}

#[test]
fn plain_subscribe_uses_normal_priority() {
    let bus = EventBus::new();
    let log = new_log();
    let _low = log_on::<Ping>(&bus, EventPriority::Low, &log, "low");
    let plain = {
        let log = Arc::clone(&log);
        bus.subscribe(move |_: &Ping| log.lock().unwrap().push("plain"))
    };
    let _high = log_on::<Ping>(&bus, EventPriority::High, &log, "high");

    bus.publish(&Ping);
    assert_eq!(entries(&log), ["high", "plain", "low"]);
    drop(plain);
}

#[test]
fn dropping_a_subscription_unsubscribes() {
    let bus = EventBus::new();
    let log = new_log();
    let subscription = log_on::<Ping>(&bus, EventPriority::Normal, &log, "ping");
    assert_eq!(bus.publish(&Ping), 1);

    drop(subscription);
    assert_eq!(bus.publish(&Ping), 0);
    assert_eq!(entries(&log), ["ping"]);
    assert_eq!(bus.subscriber_count::<Ping>(), 0);
}

#[test]
fn detach_keeps_the_handler_for_the_life_of_the_bus() {
    let bus = EventBus::new();
    let log = new_log();
    log_on::<Ping>(&bus, EventPriority::Normal, &log, "ping").detach();

    assert_eq!(bus.publish(&Ping), 1);
    assert_eq!(bus.publish(&Ping), 1);
    assert_eq!(bus.subscriber_count::<Ping>(), 1);
}

#[test]
fn unsubscribe_reports_whether_it_removed_the_handler() {
    let bus = EventBus::new();
    let subscription = bus.subscribe(|_: &Ping| {});
    assert!(subscription.unsubscribe());
    assert_eq!(bus.subscriber_count::<Ping>(), 0);

    let orphan = bus.subscribe(|_: &Ping| {});
    drop(bus);
    assert!(!orphan.unsubscribe());
}

#[test]
fn is_active_follows_the_subscription_and_the_bus() {
    let bus = EventBus::new();
    let other_handle = bus.clone();
    let subscription = bus.subscribe(|_: &Ping| {});
    assert!(subscription.is_active());

    drop(bus);
    assert!(
        subscription.is_active(),
        "another handle keeps the bus alive"
    );

    drop(other_handle);
    assert!(!subscription.is_active());
}

#[test]
fn subscription_ids_are_distinct() {
    let bus = EventBus::new();
    let first = bus.subscribe(|_: &Ping| {});
    let second = bus.subscribe(|_: &Pong| {});
    assert_ne!(first.id(), second.id());
}

#[test]
fn subscriber_count_tracks_each_event_type() {
    let bus = EventBus::new();
    assert_eq!(bus.subscriber_count::<Ping>(), 0);

    let first = bus.subscribe(|_: &Ping| {});
    let _second = bus.subscribe(|_: &Ping| {});
    let _other = bus.subscribe(|_: &Pong| {});
    assert_eq!(bus.subscriber_count::<Ping>(), 2);
    assert_eq!(bus.subscriber_count::<Pong>(), 1);

    drop(first);
    assert_eq!(bus.subscriber_count::<Ping>(), 1);
}

#[test]
fn clones_share_one_bus() {
    let bus = EventBus::new();
    let clone = bus.clone();
    let log = new_log();
    let _subscription = log_on::<Ping>(&clone, EventPriority::Normal, &log, "ping");

    assert_eq!(bus.publish(&Ping), 1);
    assert_eq!(bus.subscriber_count::<Ping>(), 1);
}

#[test]
fn a_weak_handle_upgrades_only_while_a_bus_lives() {
    let bus = EventBus::new();
    let weak: WeakEventBus = bus.downgrade();
    assert!(weak.upgrade().is_some());

    let clone = bus.clone();
    drop(bus);
    assert!(weak.upgrade().is_some());

    drop(clone);
    assert!(weak.upgrade().is_none());
}

#[test]
fn a_handler_unsubscribed_during_a_publish_is_not_called_by_it() {
    let bus = EventBus::new();
    let log = new_log();
    let victim: Arc<Mutex<Option<Subscription>>> = Arc::new(Mutex::new(None));

    let killer = {
        let victim = Arc::clone(&victim);
        bus.subscribe_with_priority(EventPriority::High, move |_: &Ping| {
            drop(victim.lock().unwrap().take());
        })
    };
    *victim.lock().unwrap() = Some(log_on::<Ping>(&bus, EventPriority::Normal, &log, "victim"));

    assert_eq!(bus.publish(&Ping), 1);
    assert!(entries(&log).is_empty());
    assert_eq!(bus.subscriber_count::<Ping>(), 1);
    drop(killer);
}

#[test]
fn a_handler_subscribed_during_a_publish_waits_for_the_next_one() {
    let bus = EventBus::new();
    let late_calls = Arc::new(AtomicUsize::new(0));
    let kept: Arc<Mutex<Vec<Subscription>>> = Arc::new(Mutex::new(Vec::new()));

    let weak = bus.downgrade();
    let adder = {
        let late_calls = Arc::clone(&late_calls);
        let kept = Arc::clone(&kept);
        bus.subscribe(move |_: &Ping| {
            if let Some(bus) = weak.upgrade() {
                let late_calls = Arc::clone(&late_calls);
                let late = bus.subscribe(move |_: &Ping| {
                    late_calls.fetch_add(1, Ordering::SeqCst);
                });
                kept.lock().unwrap().push(late);
            }
        })
    };

    assert_eq!(
        bus.publish(&Ping),
        1,
        "the handler added during this publish is not called"
    );
    assert_eq!(late_calls.load(Ordering::SeqCst), 0);

    assert_eq!(
        bus.publish(&Ping),
        2,
        "now the first late handler takes part"
    );
    assert_eq!(late_calls.load(Ordering::SeqCst), 1);
    assert_eq!(kept.lock().unwrap().len(), 2);
    drop(adder);
}

#[test]
fn a_handler_can_publish_another_event() {
    let bus = EventBus::new();
    let log = new_log();
    let _pong = log_on::<Pong>(&bus, EventPriority::Normal, &log, "pong");

    let weak = bus.downgrade();
    let _ping = bus.subscribe(move |_: &Ping| {
        if let Some(bus) = weak.upgrade() {
            bus.publish(&Pong);
        }
    });

    assert_eq!(bus.publish(&Ping), 1);
    assert_eq!(entries(&log), ["pong"]);
}

#[test]
fn a_handler_can_publish_the_same_event_again() {
    let bus = EventBus::new();
    let hits = Arc::new(AtomicUsize::new(0));

    let weak = bus.downgrade();
    let counter = Arc::clone(&hits);
    let _subscription = bus.subscribe(move |countdown: &Countdown| {
        counter.fetch_add(1, Ordering::SeqCst);
        if let (Some(next), Some(bus)) = (countdown.0.checked_sub(1), weak.upgrade()) {
            bus.publish(&Countdown(next));
        }
    });

    bus.publish(&Countdown(5));
    assert_eq!(hits.load(Ordering::SeqCst), 6);
}

#[test]
fn a_panicking_handler_unwinds_through_publish_and_leaves_the_bus_usable() {
    let bus = EventBus::new();
    let log = new_log();
    let _boom = bus.subscribe_with_priority(EventPriority::High, |_: &Ping| panic!("boom"));
    let _after = log_on::<Ping>(&bus, EventPriority::Normal, &log, "after");

    let result = catch_unwind(AssertUnwindSafe(|| bus.publish(&Ping)));
    assert!(result.is_err());
    assert!(
        entries(&log).is_empty(),
        "handlers behind the panicking one are skipped"
    );

    let _fresh = bus.subscribe(|_: &Pong| {});
    assert_eq!(bus.publish(&Pong), 1);
    assert_eq!(bus.subscriber_count::<Ping>(), 2);
}

#[test]
fn dropping_a_handler_that_owns_a_subscription_does_not_deadlock() {
    within_patience(
        "unsubscribing a handler that owns another subscription",
        || {
            let bus = EventBus::new();
            let inner = bus.subscribe(|_: &Pong| {});
            let outer = bus.subscribe(move |_: &Ping| {
                let _owned = &inner;
            });
            assert_eq!(bus.subscriber_count::<Pong>(), 1);

            assert!(outer.unsubscribe());
            assert_eq!(bus.subscriber_count::<Pong>(), 0);
        },
    );
}

#[test]
fn dropping_the_last_bus_handle_drops_the_handlers() {
    let bus = EventBus::new();
    let token = Arc::new(());
    let captured = Arc::clone(&token);
    let subscription = bus.subscribe(move |_: &Ping| {
        let _held = &captured;
    });
    assert_eq!(Arc::strong_count(&token), 2);

    drop(bus);
    assert_eq!(Arc::strong_count(&token), 1);
    drop(subscription);
}

#[test]
fn the_bus_types_can_cross_threads() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<EventBus>();
    assert_send_sync::<WeakEventBus>();
    assert_send_sync::<Subscription>();
}

#[test]
fn many_threads_can_publish_at_once() {
    let bus = EventBus::new();
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&hits);
    let _subscription = bus.subscribe(move |_: &Ping| {
        counter.fetch_add(1, Ordering::SeqCst);
    });

    let rounds = scaled(1000, 25);
    thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| {
                for _ in 0..rounds {
                    bus.publish(&Ping);
                }
            });
        }
    });
    assert_eq!(hits.load(Ordering::SeqCst), 4 * rounds as usize);
}

#[test]
fn subscribing_and_unsubscribing_while_others_publish_is_safe() {
    let bus = EventBus::new();
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&hits);
    let _permanent = bus.subscribe(move |_: &Ping| {
        counter.fetch_add(1, Ordering::SeqCst);
    });

    let publishes = scaled(5000, 100);
    let churns = scaled(2000, 40);
    thread::scope(|scope| {
        scope.spawn(|| {
            for _ in 0..publishes {
                bus.publish(&Ping);
            }
        });
        scope.spawn(|| {
            for _ in 0..churns {
                let churn = bus.subscribe(|_: &Ping| {});
                drop(churn);
            }
        });
    });

    assert_eq!(hits.load(Ordering::SeqCst), publishes as usize);
    assert_eq!(bus.subscriber_count::<Ping>(), 1);
}
