//! Delivery as jobs on the `threading` pool: `publish_parallel` and `dispatch_parallel` (E4).

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use event::{EventBus, EventPriority, Flow, Subscription};
use threading::{PoolConfig, ThreadPool, WorkerCount};

struct Ping;
struct Pong;
struct Number(u32);

fn make_pool(workers: usize) -> ThreadPool {
    ThreadPool::new(PoolConfig::new().with_workers(WorkerCount::Exact(workers))).unwrap()
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

/// Subscribes an observer that adds one to `counter` for every `E` it receives.
fn count_on<E: 'static>(bus: &EventBus, counter: &Arc<AtomicUsize>) -> Subscription {
    let counter = Arc::clone(counter);
    bus.subscribe(move |_: &E| {
        counter.fetch_add(1, Ordering::SeqCst);
    })
}

// ----- publish_parallel -------------------------------------------------------------------------

#[test]
fn every_observer_runs_once_and_sees_the_shared_event() {
    let pool = make_pool(3);
    let bus = EventBus::new();
    let total = Arc::new(AtomicUsize::new(0));

    let subscriptions: Vec<Subscription> = (0..5)
        .map(|_| {
            let total = Arc::clone(&total);
            bus.subscribe(move |number: &Number| {
                total.fetch_add(number.0 as usize, Ordering::SeqCst);
            })
        })
        .collect();

    assert_eq!(bus.publish_parallel(&pool.spawner(), &Number(7)), 5);
    assert_eq!(total.load(Ordering::SeqCst), 35);
    drop(subscriptions);
}

#[test]
fn nobody_listening_means_nothing_runs() {
    let pool = make_pool(2);
    let bus = EventBus::new();
    assert_eq!(bus.publish_parallel(&pool.spawner(), &Ping), 0);
}

#[test]
fn interceptors_are_skipped() {
    let pool = make_pool(2);
    let bus = EventBus::new();
    let intercepted = Arc::new(AtomicUsize::new(0));
    let observed = Arc::new(AtomicUsize::new(0));

    let counter = Arc::clone(&intercepted);
    let _interceptor = bus.subscribe_mut(move |_: &mut Ping| {
        counter.fetch_add(1, Ordering::SeqCst);
        Flow::Continue
    });
    let _observer = count_on::<Ping>(&bus, &observed);

    assert_eq!(bus.publish_parallel(&pool.spawner(), &Ping), 1);
    assert_eq!(intercepted.load(Ordering::SeqCst), 0);
    assert_eq!(observed.load(Ordering::SeqCst), 1);
}

#[test]
fn priorities_are_phases_with_a_barrier_between_them() {
    let pool = make_pool(3);
    let bus = EventBus::new();
    let log = Arc::new(Mutex::new(Vec::new()));

    let _subscriptions = [
        (EventPriority::Low, "low"),
        (EventPriority::High, "high"),
        (EventPriority::Normal, "normal"),
        (EventPriority::High, "high"),
    ]
    .map(|(priority, what)| {
        let log = Arc::clone(&log);
        bus.subscribe_with_priority(priority, move |_: &Ping| {
            thread::sleep(Duration::from_millis(30));
            log.lock().unwrap().push(what);
        })
    });

    assert_eq!(bus.publish_parallel(&pool.spawner(), &Ping), 4);
    assert_eq!(*log.lock().unwrap(), ["high", "high", "normal", "low"]);
}

#[test]
fn observers_of_one_priority_run_at_the_same_time() {
    within_patience("two observers that wait for each other", || {
        let pool = make_pool(3);
        let bus = EventBus::new();
        let barrier = Arc::new(Barrier::new(2));

        let _subscriptions = [(), ()].map(|()| {
            let barrier = Arc::clone(&barrier);
            bus.subscribe(move |_: &Ping| {
                barrier.wait();
            })
        });

        assert_eq!(bus.publish_parallel(&pool.spawner(), &Ping), 2);
    });
}

#[test]
fn a_one_shot_handler_runs_once() {
    let pool = make_pool(2);
    let bus = EventBus::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let _once = bus.subscribe_once(move |_: &Ping| {
        counter.fetch_add(1, Ordering::SeqCst);
    });

    assert_eq!(bus.publish_parallel(&pool.spawner(), &Ping), 1);
    assert_eq!(bus.publish_parallel(&pool.spawner(), &Ping), 0);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(bus.subscriber_count::<Ping>(), 0);
}

#[test]
fn a_pool_without_workers_runs_everything_on_the_calling_thread() {
    let pool = make_pool(0);
    let bus = EventBus::new();
    let threads = Arc::new(Mutex::new(Vec::new()));

    let _subscriptions = [(), (), ()].map(|()| {
        let threads = Arc::clone(&threads);
        bus.subscribe(move |_: &Ping| {
            threads.lock().unwrap().push(thread::current().id());
        })
    });

    assert_eq!(bus.publish_parallel(&pool.spawner(), &Ping), 3);
    let here = thread::current().id();
    let threads = threads.lock().unwrap();
    assert_eq!(threads.len(), 3);
    assert!(threads.iter().all(|id| *id == here));
}

#[test]
fn a_panicking_observer_lets_its_phase_finish_and_stops_the_later_phases() {
    let pool = make_pool(3);
    let bus = EventBus::new();
    let same_phase = Arc::new(AtomicUsize::new(0));
    let later_phase = Arc::new(AtomicUsize::new(0));

    let _boom = bus.subscribe_with_priority(EventPriority::High, |_: &Ping| panic!("boom"));
    let counter = Arc::clone(&same_phase);
    let _sibling = bus.subscribe_with_priority(EventPriority::High, move |_: &Ping| {
        counter.fetch_add(1, Ordering::SeqCst);
    });
    let counter = Arc::clone(&later_phase);
    let _later = bus.subscribe_with_priority(EventPriority::Normal, move |_: &Ping| {
        counter.fetch_add(1, Ordering::SeqCst);
    });

    let spawner = pool.spawner();
    let result = catch_unwind(AssertUnwindSafe(|| bus.publish_parallel(&spawner, &Ping)));
    assert!(result.is_err());
    assert_eq!(same_phase.load(Ordering::SeqCst), 1);
    assert_eq!(later_phase.load(Ordering::SeqCst), 0);
}

#[test]
fn it_can_be_called_from_inside_a_job() {
    within_patience("publish_parallel from a job on a small pool", || {
        let pool = make_pool(2);
        let bus = EventBus::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let _subscriptions = [(), (), ()].map(|()| count_on::<Ping>(&bus, &calls));

        let spawner = pool.spawner();
        let bus_for_job = bus.clone();
        let handle = pool.spawn(move || bus_for_job.publish_parallel(&spawner, &Ping));
        assert_eq!(handle.wait(), 3);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    });
}

// ----- dispatch_parallel ------------------------------------------------------------------------

#[test]
fn an_empty_queue_dispatches_nothing_in_parallel() {
    let pool = make_pool(2);
    let bus = EventBus::new();
    assert_eq!(bus.dispatch_parallel(&pool.spawner()), 0);
}

#[test]
fn every_queued_event_is_delivered_and_counted() {
    let pool = make_pool(3);
    let bus = EventBus::new();
    let numbers = Arc::new(AtomicUsize::new(0));
    let pongs = Arc::new(AtomicUsize::new(0));
    let _numbers = count_on::<Number>(&bus, &numbers);
    let _pongs = count_on::<Pong>(&bus, &pongs);

    for value in 0..3 {
        bus.post(Number(value));
    }
    bus.post(Pong);
    bus.post(Pong);
    bus.post(Ping);

    assert_eq!(bus.dispatch_parallel(&pool.spawner()), 6);
    assert_eq!(numbers.load(Ordering::SeqCst), 3);
    assert_eq!(pongs.load(Ordering::SeqCst), 2);
    assert_eq!(bus.pending(), 0);
}

#[test]
fn events_of_one_type_keep_their_posting_order() {
    let pool = make_pool(3);
    let bus = EventBus::new();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let _numbers = bus.subscribe(move |number: &Number| sink.lock().unwrap().push(number.0));
    let _pongs = bus.subscribe(|_: &Pong| {});

    for value in 0..100 {
        bus.post(Number(value));
        bus.post(Pong);
    }

    assert_eq!(bus.dispatch_parallel(&pool.spawner()), 200);
    assert_eq!(*seen.lock().unwrap(), (0..100).collect::<Vec<u32>>());
}

#[test]
fn events_of_different_types_are_delivered_at_the_same_time() {
    within_patience("two event types that wait for each other", || {
        let pool = make_pool(3);
        let bus = EventBus::new();
        let barrier = Arc::new(Barrier::new(2));

        let wait = Arc::clone(&barrier);
        let _numbers = bus.subscribe(move |_: &Number| {
            wait.wait();
        });
        let wait = Arc::clone(&barrier);
        let _pongs = bus.subscribe(move |_: &Pong| {
            wait.wait();
        });

        bus.post(Number(1));
        bus.post(Pong);
        assert_eq!(bus.dispatch_parallel(&pool.spawner()), 2);
    });
}

#[test]
fn queued_events_reach_every_kind_of_handler_in_parallel_too() {
    let pool = make_pool(2);
    let bus = EventBus::new();
    let observed = Arc::new(AtomicUsize::new(0));
    let intercepted = Arc::new(AtomicUsize::new(0));
    let once = Arc::new(AtomicUsize::new(0));

    let _observer = count_on::<Ping>(&bus, &observed);
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
    assert_eq!(bus.dispatch_parallel(&pool.spawner()), 2);
    assert_eq!(observed.load(Ordering::SeqCst), 2);
    assert_eq!(intercepted.load(Ordering::SeqCst), 2);
    assert_eq!(once.load(Ordering::SeqCst), 1);
}

#[test]
fn a_panicking_handler_loses_only_its_own_event() {
    let pool = make_pool(3);
    let spawner = pool.spawner();
    let bus = EventBus::new();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let pongs = Arc::new(AtomicUsize::new(0));

    let sink = Arc::clone(&seen);
    let _numbers = bus.subscribe(move |number: &Number| {
        sink.lock().unwrap().push(number.0);
        if number.0 == 2 {
            panic!("boom");
        }
    });
    let _pongs = count_on::<Pong>(&bus, &pongs);

    bus.post(Number(1));
    bus.post(Number(2));
    bus.post(Number(3));
    bus.post(Pong);

    let result = catch_unwind(AssertUnwindSafe(|| bus.dispatch_parallel(&spawner)));
    assert!(result.is_err());
    assert_eq!(
        *seen.lock().unwrap(),
        [1, 2, 3],
        "the events behind the panicking one are delivered"
    );
    assert_eq!(pongs.load(Ordering::SeqCst), 1);
    assert_eq!(bus.pending(), 0);

    bus.post(Ping);
    assert_eq!(
        bus.dispatch_parallel(&spawner),
        1,
        "the pass flag was released"
    );
}

#[test]
fn a_nested_dispatch_returns_zero() {
    let pool = make_pool(2);
    let spawner = pool.spawner();
    let bus = EventBus::new();
    let nested = Arc::new(AtomicUsize::new(0));

    let total = Arc::clone(&nested);
    let weak = bus.downgrade();
    let inner_spawner = spawner.clone();
    let _subscription = bus.subscribe(move |_: &Ping| {
        if let Some(bus) = weak.upgrade() {
            total.fetch_add(
                bus.dispatch() + bus.dispatch_parallel(&inner_spawner),
                Ordering::SeqCst,
            );
        }
    });

    bus.post(Ping);
    assert_eq!(bus.dispatch_parallel(&spawner), 1);
    assert_eq!(nested.load(Ordering::SeqCst), 0);
}

#[test]
fn a_pool_without_workers_delivers_in_posting_order() {
    let pool = make_pool(0);
    let bus = EventBus::new();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let _numbers = bus.subscribe(move |number: &Number| sink.lock().unwrap().push(number.0));

    for value in 0..10 {
        bus.post(Number(value));
    }

    assert_eq!(bus.dispatch_parallel(&pool.spawner()), 10);
    assert_eq!(*seen.lock().unwrap(), (0..10).collect::<Vec<u32>>());
}

#[test]
fn producers_and_a_parallel_dispatcher_lose_nothing() {
    within_patience("a parallel dispatcher while two threads post", || {
        let pool = make_pool(3);
        let spawner = pool.spawner();
        let bus = EventBus::new();
        let numbers = Arc::new(AtomicUsize::new(0));
        let pongs = Arc::new(AtomicUsize::new(0));
        let _numbers = count_on::<Number>(&bus, &numbers);
        let _pongs = count_on::<Pong>(&bus, &pongs);

        let per_producer = scaled(1000, 15);
        let expected = 2 * per_producer as usize;
        thread::scope(|scope| {
            let sender = bus.sender();
            scope.spawn(move || {
                for value in 0..per_producer {
                    sender.post(Number(value)).unwrap();
                }
            });
            let sender = bus.sender();
            scope.spawn(move || {
                for _ in 0..per_producer {
                    sender.post(Pong).unwrap();
                }
            });

            let mut total = 0;
            while total < expected {
                total += bus.dispatch_parallel(&spawner);
                thread::yield_now();
            }
        });

        assert_eq!(numbers.load(Ordering::SeqCst), per_producer as usize);
        assert_eq!(pongs.load(Ordering::SeqCst), per_producer as usize);
        assert_eq!(bus.pending(), 0);
    });
}
