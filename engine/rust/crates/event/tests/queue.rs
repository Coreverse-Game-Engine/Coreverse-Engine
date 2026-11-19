//! Queued events: `post`, `dispatch`, `EventSender` (E3).

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use event::{EventBus, EventPriority, EventSender, Flow, PostError, Subscription};

struct Number(u32);
struct Word(&'static str);
struct Ping;
struct Tagged(u32, u32);
struct Tracked(Arc<()>);

/// Subscribes a handler that collects the values of the `Number`s it receives.
fn collect(bus: &EventBus) -> (Arc<Mutex<Vec<u32>>>, Subscription) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let subscription = bus.subscribe(move |number: &Number| sink.lock().unwrap().push(number.0));
    (seen, subscription)
}

/// Runs `work` on its own thread and fails the test if it has not finished after 20 seconds (a deadlock).
fn within_patience<T: Send + 'static>(what: &str, work: impl FnOnce() -> T + Send + 'static) -> T {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let _ = sender.send(work());
    });
    receiver
        .recv_timeout(Duration::from_secs(20))
        .unwrap_or_else(|_| panic!("{what} did not finish within 20 seconds (deadlock or panic)"))
}

#[test]
fn posting_runs_nothing_until_dispatch() {
    let bus = EventBus::new();
    let (seen, _subscription) = collect(&bus);

    bus.post(Number(1));
    assert!(seen.lock().unwrap().is_empty());
    assert_eq!(bus.pending(), 1);

    assert_eq!(bus.dispatch(), 1);
    assert_eq!(*seen.lock().unwrap(), [1]);
    assert_eq!(bus.pending(), 0);
}

#[test]
fn an_empty_queue_dispatches_nothing() {
    let bus = EventBus::new();
    assert_eq!(bus.dispatch(), 0);
    assert_eq!(bus.pending(), 0);
}

#[test]
fn events_are_delivered_in_posting_order_across_types() {
    let bus = EventBus::new();
    let log = Arc::new(Mutex::new(Vec::new()));

    let sink = Arc::clone(&log);
    let _numbers = bus.subscribe(move |number: &Number| {
        sink.lock().unwrap().push(format!("n{}", number.0));
    });
    let sink = Arc::clone(&log);
    let _words = bus.subscribe(move |word: &Word| {
        sink.lock().unwrap().push(format!("w{}", word.0));
    });

    bus.post(Number(1));
    bus.post(Word("a"));
    bus.post(Number(2));
    assert_eq!(bus.dispatch(), 3);
    assert_eq!(*log.lock().unwrap(), ["n1", "wa", "n2"]);
}

#[test]
fn an_event_nobody_listens_to_still_counts_as_delivered() {
    let bus = EventBus::new();
    bus.post(Ping);
    bus.post(Ping);
    bus.post(Ping);

    assert_eq!(bus.dispatch(), 3);
    assert_eq!(bus.pending(), 0);
}

#[test]
fn handlers_subscribed_after_the_post_receive_the_event() {
    let bus = EventBus::new();
    bus.post(Number(5));

    let (seen, _subscription) = collect(&bus);
    assert_eq!(bus.dispatch(), 1);
    assert_eq!(*seen.lock().unwrap(), [5]);
}

#[test]
fn a_handler_unsubscribed_before_dispatch_is_not_called() {
    let bus = EventBus::new();
    let (seen, subscription) = collect(&bus);
    bus.post(Number(5));

    drop(subscription);
    assert_eq!(bus.dispatch(), 1);
    assert!(seen.lock().unwrap().is_empty());
}

#[test]
fn queued_events_reach_observers_interceptors_and_one_shot_handlers() {
    let bus = EventBus::new();
    let observed = Arc::new(AtomicUsize::new(0));
    let intercepted = Arc::new(AtomicUsize::new(0));
    let once = Arc::new(AtomicUsize::new(0));

    let counter = Arc::clone(&observed);
    let _observer = bus.subscribe(move |_: &Ping| {
        counter.fetch_add(1, Ordering::SeqCst);
    });
    let counter = Arc::clone(&intercepted);
    let _interceptor = bus.subscribe_mut(move |_: &mut Ping| {
        counter.fetch_add(1, Ordering::SeqCst);
        Flow::Continue
    });
    let counter = Arc::clone(&once);
    let _once = bus.subscribe_once(move |_: &Ping| {
        counter.fetch_add(1, Ordering::SeqCst);
    });

    bus.post(Ping);
    bus.post(Ping);
    assert_eq!(bus.dispatch(), 2);
    assert_eq!(observed.load(Ordering::SeqCst), 2);
    assert_eq!(intercepted.load(Ordering::SeqCst), 2);
    assert_eq!(once.load(Ordering::SeqCst), 1);
}

#[test]
fn stop_ends_the_chain_for_a_queued_event() {
    let bus = EventBus::new();
    let observed = Arc::new(AtomicUsize::new(0));

    let _stop = bus.subscribe_mut_with_priority(EventPriority::High, |_: &mut Ping| Flow::Stop);
    let counter = Arc::clone(&observed);
    let _observer = bus.subscribe(move |_: &Ping| {
        counter.fetch_add(1, Ordering::SeqCst);
    });

    bus.post(Ping);
    assert_eq!(bus.dispatch(), 1);
    assert_eq!(observed.load(Ordering::SeqCst), 0);
}

#[test]
fn events_posted_during_a_dispatch_wait_for_the_next_one() {
    let bus = EventBus::new();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let weak = bus.downgrade();
    let _chain = bus.subscribe(move |number: &Number| {
        sink.lock().unwrap().push(number.0);
        if let (Some(bus), true) = (weak.upgrade(), number.0 < 3) {
            bus.post(Number(number.0 + 1));
        }
    });

    bus.post(Number(0));
    for expected_so_far in 1..=4_usize {
        assert_eq!(bus.dispatch(), 1, "one event per pass");
        assert_eq!(seen.lock().unwrap().len(), expected_so_far);
    }
    assert_eq!(bus.pending(), 0);
    assert_eq!(*seen.lock().unwrap(), [0, 1, 2, 3]);
}

#[test]
fn a_nested_dispatch_returns_zero_and_the_outer_pass_continues() {
    let bus = EventBus::new();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let nested = Arc::new(AtomicUsize::new(0));

    let sink = Arc::clone(&seen);
    let nested_total = Arc::clone(&nested);
    let weak = bus.downgrade();
    let _subscription = bus.subscribe(move |number: &Number| {
        sink.lock().unwrap().push(number.0);
        if let Some(bus) = weak.upgrade() {
            nested_total.fetch_add(bus.dispatch(), Ordering::SeqCst);
        }
    });

    bus.post(Number(1));
    bus.post(Number(2));
    assert_eq!(bus.dispatch(), 2);
    assert_eq!(nested.load(Ordering::SeqCst), 0);
    assert_eq!(*seen.lock().unwrap(), [1, 2]);
}

#[test]
fn clear_pending_discards_events_without_delivering_them() {
    let bus = EventBus::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let _subscription = bus.subscribe(move |tracked: &Tracked| {
        assert!(Arc::strong_count(&tracked.0) >= 1);
        counter.fetch_add(1, Ordering::SeqCst);
    });

    let token = Arc::new(());
    bus.post(Tracked(Arc::clone(&token)));
    bus.post(Tracked(Arc::clone(&token)));
    assert_eq!(Arc::strong_count(&token), 3);

    assert_eq!(bus.clear_pending(), 2);
    assert_eq!(Arc::strong_count(&token), 1);
    assert_eq!(bus.pending(), 0);
    assert_eq!(bus.dispatch(), 0);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn a_panicking_handler_loses_only_its_own_event() {
    let bus = EventBus::new();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let _subscription = bus.subscribe(move |number: &Number| {
        sink.lock().unwrap().push(number.0);
        if number.0 == 1 {
            panic!("boom");
        }
    });

    bus.post(Number(1));
    bus.post(Number(2));
    bus.post(Number(3));

    let result = catch_unwind(AssertUnwindSafe(|| bus.dispatch()));
    assert!(result.is_err());
    assert_eq!(*seen.lock().unwrap(), [1]);
    assert_eq!(
        bus.pending(),
        2,
        "the events behind the panicking one are kept"
    );

    assert_eq!(
        bus.dispatch(),
        2,
        "and the next dispatch delivers them in order"
    );
    assert_eq!(*seen.lock().unwrap(), [1, 2, 3]);
}

#[test]
fn a_clone_of_the_bus_shares_the_queue() {
    let bus = EventBus::new();
    let clone = bus.clone();
    let (seen, _subscription) = collect(&bus);

    clone.post(Number(8));
    assert_eq!(bus.pending(), 1);
    assert_eq!(bus.dispatch(), 1);
    assert_eq!(*seen.lock().unwrap(), [8]);
}

#[test]
fn events_posted_from_other_threads_keep_each_threads_order() {
    let bus = EventBus::new();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let _subscription = bus.subscribe(move |tagged: &Tagged| {
        sink.lock().unwrap().push((tagged.0, tagged.1));
    });

    let sender = bus.sender();
    thread::scope(|scope| {
        for thread_id in 0..4 {
            let sender = sender.clone();
            scope.spawn(move || {
                for sequence in 0..500 {
                    sender.post(Tagged(thread_id, sequence)).unwrap();
                }
            });
        }
    });

    assert_eq!(bus.dispatch(), 2000);
    let seen = seen.lock().unwrap();
    for thread_id in 0..4 {
        let sequences: Vec<u32> = seen
            .iter()
            .filter(|(id, _)| *id == thread_id)
            .map(|(_, sequence)| *sequence)
            .collect();
        assert_eq!(sequences, (0..500).collect::<Vec<u32>>());
    }
}

#[test]
fn dispatching_while_producers_post_loses_nothing() {
    within_patience("dispatching while two threads post", || {
        let bus = EventBus::new();
        let delivered = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&delivered);
        let _subscription = bus.subscribe(move |_: &Ping| {
            counter.fetch_add(1, Ordering::SeqCst);
        });

        thread::scope(|scope| {
            for _ in 0..2 {
                let sender = bus.sender();
                scope.spawn(move || {
                    for _ in 0..1000 {
                        sender.post(Ping).unwrap();
                    }
                });
            }

            let mut total = 0;
            while total < 2000 {
                total += bus.dispatch();
                thread::yield_now();
            }
        });

        assert_eq!(delivered.load(Ordering::SeqCst), 2000);
        assert_eq!(bus.pending(), 0);
    });
}

#[test]
fn two_threads_dispatching_deliver_every_event_once_and_in_order() {
    let bus = EventBus::new();
    let (seen, _subscription) = collect(&bus);
    for value in 0..2000 {
        bus.post(Number(value));
    }

    thread::scope(|scope| {
        for _ in 0..2 {
            scope.spawn(|| {
                for _ in 0..50 {
                    bus.dispatch();
                }
            });
        }
    });
    while bus.pending() > 0 {
        bus.dispatch();
    }

    assert_eq!(*seen.lock().unwrap(), (0..2000).collect::<Vec<u32>>());
}

#[test]
fn a_sender_posts_until_the_bus_is_gone() {
    let bus = EventBus::new();
    let (seen, _subscription) = collect(&bus);
    let sender = bus.sender();
    assert!(sender.is_connected());

    assert!(sender.post(Number(1)).is_ok());
    assert_eq!(bus.dispatch(), 1);
    assert_eq!(*seen.lock().unwrap(), [1]);

    drop(bus);
    assert!(!sender.is_connected());
    let error: PostError<Number> = sender.post(Number(9)).unwrap_err();
    assert_eq!(error.to_string(), "the event bus no longer exists");
    assert_eq!(error.into_inner().0, 9);
}

#[test]
fn a_sender_does_not_keep_the_bus_alive() {
    let bus = EventBus::new();
    let weak = bus.downgrade();
    let sender = bus.sender();

    drop(bus);
    assert!(weak.upgrade().is_none());
    assert!(!sender.is_connected());
}

#[test]
fn queued_events_are_dropped_with_the_bus() {
    let bus = EventBus::new();
    let token = Arc::new(());
    bus.post(Tracked(Arc::clone(&token)));
    assert_eq!(Arc::strong_count(&token), 2);

    drop(bus);
    assert_eq!(Arc::strong_count(&token), 1);
}

#[test]
fn the_sender_types_can_cross_threads() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<EventSender>();
    assert_send_sync::<PostError<u32>>();
}
