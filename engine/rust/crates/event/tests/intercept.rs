//! Interceptors, `publish_mut`, `Flow`, `Dispatch` and `Cancellable` (E2).

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use event::{Cancellable, Dispatch, EventBus, EventPriority, Flow, Subscription};

struct Damage(u32);
struct Ping;

type Log = Arc<Mutex<Vec<&'static str>>>;

fn new_log() -> Log {
    Arc::new(Mutex::new(Vec::new()))
}

fn entries(log: &Log) -> Vec<&'static str> {
    log.lock().unwrap().clone()
}

/// Subscribes an interceptor that logs `what` and answers `flow`.
fn log_mut<E: 'static>(
    bus: &EventBus,
    priority: EventPriority,
    log: &Log,
    what: &'static str,
    flow: Flow,
) -> Subscription {
    let log = Arc::clone(log);
    bus.subscribe_mut_with_priority(priority, move |_: &mut E| {
        log.lock().unwrap().push(what);
        flow
    })
}

#[test]
fn an_interceptor_can_change_the_event() {
    let bus = EventBus::new();
    let _armor = bus.subscribe_mut(|damage: &mut Damage| {
        damage.0 /= 2;
        Flow::Continue
    });

    let mut hit = Damage(10);
    let dispatch = bus.publish_mut(&mut hit);
    assert_eq!(hit.0, 5);
    assert_eq!(dispatch.called(), 1);
    assert!(!dispatch.was_stopped());
}

#[test]
fn publishing_without_handlers_reports_nothing() {
    let bus = EventBus::new();
    let dispatch = bus.publish_mut(&mut Damage(1));
    assert_eq!(dispatch, Dispatch::default());
    assert_eq!(dispatch.called(), 0);
    assert!(!dispatch.was_stopped());
}

#[test]
fn interceptors_run_in_priority_order_and_see_earlier_changes() {
    let bus = EventBus::new();
    let _low = bus.subscribe_mut_with_priority(EventPriority::Low, |damage: &mut Damage| {
        damage.0 += 1;
        Flow::Continue
    });
    let _normal = bus.subscribe_mut(|damage: &mut Damage| {
        damage.0 *= 2;
        Flow::Continue
    });
    let _high = bus.subscribe_mut_with_priority(EventPriority::High, |damage: &mut Damage| {
        damage.0 += 10;
        Flow::Continue
    });

    let mut hit = Damage(1);
    let dispatch = bus.publish_mut(&mut hit);
    assert_eq!(hit.0, 23, "(1 + 10) * 2 + 1");
    assert_eq!(dispatch.called(), 3);
}

#[test]
fn stop_ends_the_chain() {
    let bus = EventBus::new();
    let log = new_log();
    let _high = log_mut::<Ping>(&bus, EventPriority::High, &log, "high", Flow::Stop);
    let _normal = log_mut::<Ping>(&bus, EventPriority::Normal, &log, "normal", Flow::Continue);

    let dispatch = bus.publish_mut(&mut Ping);
    assert_eq!(entries(&log), ["high"]);
    assert_eq!(dispatch.called(), 1);
    assert!(dispatch.was_stopped());
}

#[test]
fn continue_lets_every_handler_run() {
    let bus = EventBus::new();
    let log = new_log();
    let _high = log_mut::<Ping>(&bus, EventPriority::High, &log, "high", Flow::Continue);
    let _normal = log_mut::<Ping>(&bus, EventPriority::Normal, &log, "normal", Flow::Continue);
    let _low = log_mut::<Ping>(&bus, EventPriority::Low, &log, "low", Flow::Continue);

    let dispatch = bus.publish_mut(&mut Ping);
    assert_eq!(entries(&log), ["high", "normal", "low"]);
    assert_eq!(dispatch.called(), 3);
    assert!(!dispatch.was_stopped());
}

#[test]
fn a_stopped_chain_does_not_affect_the_next_publish() {
    let bus = EventBus::new();
    let log = new_log();
    let _stop = log_mut::<Ping>(&bus, EventPriority::High, &log, "stop", Flow::Stop);

    assert!(bus.publish_mut(&mut Ping).was_stopped());
    assert!(bus.publish_mut(&mut Ping).was_stopped());
    assert_eq!(entries(&log), ["stop", "stop"]);
}

#[test]
fn observers_take_part_in_the_chain_and_see_earlier_changes() {
    let bus = EventBus::new();
    let seen = Arc::new(Mutex::new(Vec::new()));

    let _high = bus.subscribe_mut_with_priority(EventPriority::High, |damage: &mut Damage| {
        damage.0 = 7;
        Flow::Continue
    });
    let sink = Arc::clone(&seen);
    let _observer = bus.subscribe(move |damage: &Damage| sink.lock().unwrap().push(damage.0));
    let _low = bus.subscribe_mut_with_priority(EventPriority::Low, |damage: &mut Damage| {
        damage.0 = 9;
        Flow::Continue
    });

    let mut hit = Damage(1);
    let dispatch = bus.publish_mut(&mut hit);
    assert_eq!(
        *seen.lock().unwrap(),
        [7],
        "the observer runs between the two interceptors"
    );
    assert_eq!(hit.0, 9);
    assert_eq!(dispatch.called(), 3);
}

#[test]
fn a_stop_also_hides_the_event_from_later_observers() {
    let bus = EventBus::new();
    let observed = Arc::new(AtomicUsize::new(0));

    let _stop = bus.subscribe_mut_with_priority(EventPriority::High, |_: &mut Ping| Flow::Stop);
    let counter = Arc::clone(&observed);
    let _observer = bus.subscribe(move |_: &Ping| {
        counter.fetch_add(1, Ordering::SeqCst);
    });

    bus.publish_mut(&mut Ping);
    assert_eq!(observed.load(Ordering::SeqCst), 0);
}

#[test]
fn plain_publish_runs_observers_and_skips_interceptors() {
    let bus = EventBus::new();
    let intercepted = Arc::new(AtomicUsize::new(0));
    let observed = Arc::new(AtomicUsize::new(0));

    let counter = Arc::clone(&intercepted);
    let _interceptor = bus.subscribe_mut(move |_: &mut Ping| {
        counter.fetch_add(1, Ordering::SeqCst);
        Flow::Continue
    });
    let counter = Arc::clone(&observed);
    let _observer = bus.subscribe(move |_: &Ping| {
        counter.fetch_add(1, Ordering::SeqCst);
    });

    assert_eq!(bus.publish(&Ping), 1);
    assert_eq!(intercepted.load(Ordering::SeqCst), 0);
    assert_eq!(observed.load(Ordering::SeqCst), 1);
}

#[test]
fn subscriber_count_includes_interceptors() {
    let bus = EventBus::new();
    let interceptor = bus.subscribe_mut(|_: &mut Ping| Flow::Continue);
    let _observer = bus.subscribe(|_: &Ping| {});
    assert_eq!(bus.subscriber_count::<Ping>(), 2);

    drop(interceptor);
    assert_eq!(bus.subscriber_count::<Ping>(), 1);
}

#[test]
fn an_interceptor_unsubscribed_during_a_publish_is_skipped() {
    let bus = EventBus::new();
    let log = new_log();
    let victim: Arc<Mutex<Option<Subscription>>> = Arc::new(Mutex::new(None));

    let killer = {
        let victim = Arc::clone(&victim);
        bus.subscribe_mut_with_priority(EventPriority::High, move |_: &mut Ping| {
            drop(victim.lock().unwrap().take());
            Flow::Continue
        })
    };
    *victim.lock().unwrap() = Some(log_mut::<Ping>(
        &bus,
        EventPriority::Normal,
        &log,
        "victim",
        Flow::Continue,
    ));

    let dispatch = bus.publish_mut(&mut Ping);
    assert_eq!(dispatch.called(), 1);
    assert!(entries(&log).is_empty());
    drop(killer);
}

#[test]
fn a_panicking_interceptor_leaves_earlier_changes_in_the_event() {
    let bus = EventBus::new();
    let _first = bus.subscribe_mut_with_priority(EventPriority::High, |damage: &mut Damage| {
        damage.0 = 5;
        Flow::Continue
    });
    let _boom = bus.subscribe_mut(|_: &mut Damage| -> Flow { panic!("boom") });

    let mut hit = Damage(1);
    let result = catch_unwind(AssertUnwindSafe(|| {
        bus.publish_mut(&mut hit);
    }));
    assert!(result.is_err());
    assert_eq!(hit.0, 5);

    let _fresh = bus.subscribe(|_: &Ping| {});
    assert_eq!(bus.publish(&Ping), 1);
}

#[test]
fn an_interceptor_can_publish_another_event() {
    let bus = EventBus::new();
    let pings = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&pings);
    let _observer = bus.subscribe(move |_: &Ping| {
        counter.fetch_add(1, Ordering::SeqCst);
    });

    let weak = bus.downgrade();
    let _interceptor = bus.subscribe_mut(move |_: &mut Damage| {
        if let Some(bus) = weak.upgrade() {
            bus.publish(&Ping);
        }
        Flow::Continue
    });

    assert_eq!(bus.publish_mut(&mut Damage(1)).called(), 1);
    assert_eq!(pings.load(Ordering::SeqCst), 1);
}

struct Move {
    to: (i32, i32),
}

#[test]
fn a_handler_can_cancel_an_event_and_the_publisher_sees_it() {
    let bus = EventBus::new();
    let _wall = bus.subscribe_mut(|attempt: &mut Cancellable<Move>| {
        if attempt.to.0 < 0 {
            attempt.cancel();
        }
        Flow::Continue
    });

    let mut blocked = Cancellable::new(Move { to: (-1, 0) });
    bus.publish_mut(&mut blocked);
    assert!(blocked.is_cancelled());

    let mut allowed = Cancellable::new(Move { to: (1, 0) });
    bus.publish_mut(&mut allowed);
    assert!(!allowed.is_cancelled());
    assert_eq!(allowed.into_inner().to, (1, 0));
}

#[test]
fn cancelling_does_not_stop_the_chain_by_itself() {
    let bus = EventBus::new();
    let saw_cancelled = Arc::new(Mutex::new(Vec::new()));

    let _canceller =
        bus.subscribe_mut_with_priority(EventPriority::High, |attempt: &mut Cancellable<Move>| {
            attempt.cancel();
            Flow::Continue
        });
    let sink = Arc::clone(&saw_cancelled);
    let _later = bus.subscribe(move |attempt: &Cancellable<Move>| {
        sink.lock().unwrap().push(attempt.is_cancelled());
    });

    let mut attempt = Cancellable::new(Move { to: (0, 0) });
    let dispatch = bus.publish_mut(&mut attempt);
    assert_eq!(dispatch.called(), 2);
    assert_eq!(*saw_cancelled.lock().unwrap(), [true]);
}

#[test]
fn a_cancellable_reads_and_writes_the_wrapped_event_directly() {
    let mut wrapped = Cancellable::new(Damage(3));
    assert_eq!(wrapped.0, 3);
    wrapped.0 = 4;
    wrapped.cancel();
    wrapped.cancel();
    assert!(wrapped.is_cancelled());
    assert_eq!(wrapped.into_inner().0, 4);
}
