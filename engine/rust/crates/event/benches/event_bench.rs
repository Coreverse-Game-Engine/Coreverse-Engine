//! Criterion benchmarks for the `event` crate.
//!
//! Run them with `cargo bench -p event`. The groups:
//!
//! * `publish/*`, `publish_mut/*`: what one publish costs with 0, 1 and 8 observers and with 4 interceptors, and
//!   what switching the counters or the slow-handler detection on adds to it (compare `publish/8_observers`
//!   with its `_stats_on` and `_slow_detection_on` variants).
//! * `subscribe/*`: subscribing and unsubscribing (a drop), with an empty and with a busy bus.
//! * `queue/*`: posting 1000 events and dispatching them, from the bus and from an `EventSender`.
//! * `parallel/*`: the same work delivered sequentially and as jobs on a pool with four workers, to show where
//!   `publish_parallel` and `dispatch_parallel` start to pay (handlers that take a few microseconds).
//!
//! The numbers depend on the machine; compare the variants of one group with each other.
#![allow(missing_docs)]

use std::hint::black_box;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use criterion::{Criterion, criterion_group, criterion_main};
use event::{EventBus, Flow, Subscription};
use threading::{PoolConfig, ThreadPool, WorkerCount};

struct Tick(u64);
struct A;
struct B;
struct C;
struct D;

/// How many steps of busy work a "heavy" handler does; a few microseconds on a current machine.
const WORK_STEPS: u32 = 20_000;

/// Busy work that the optimizer cannot remove.
fn work(steps: u32) -> u64 {
    let mut value = 1_u64;
    for step in 0..steps {
        value = value
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(black_box(u64::from(step)));
    }
    value
}

/// Subscribes `count` cheap observers of `Tick` that add the tick to `sink`.
fn cheap_observers(bus: &EventBus, count: usize, sink: &Arc<AtomicU64>) -> Vec<Subscription> {
    (0..count)
        .map(|_| {
            let sink = Arc::clone(sink);
            bus.subscribe(move |tick: &Tick| {
                sink.fetch_add(tick.0, Ordering::Relaxed);
            })
        })
        .collect()
}

/// Subscribes an observer of `E` that does `WORK_STEPS` of busy work.
fn heavy_observer<E: 'static>(bus: &EventBus, sink: &Arc<AtomicU64>) -> Subscription {
    let sink = Arc::clone(sink);
    bus.subscribe(move |_: &E| {
        sink.fetch_add(work(WORK_STEPS), Ordering::Relaxed);
    })
}

fn publish(c: &mut Criterion) {
    let sink = Arc::new(AtomicU64::new(0));

    let empty = EventBus::new();
    c.bench_function("publish/no_subscribers", |bench| {
        bench.iter(|| empty.publish(black_box(&Tick(1))));
    });

    let one = EventBus::new();
    let _one = cheap_observers(&one, 1, &sink);
    c.bench_function("publish/1_observer", |bench| {
        bench.iter(|| one.publish(black_box(&Tick(1))));
    });

    let eight = EventBus::new();
    let _eight = cheap_observers(&eight, 8, &sink);
    c.bench_function("publish/8_observers", |bench| {
        bench.iter(|| eight.publish(black_box(&Tick(1))));
    });

    eight.set_stats_enabled(true);
    c.bench_function("publish/8_observers_stats_on", |bench| {
        bench.iter(|| eight.publish(black_box(&Tick(1))));
    });
    eight.set_stats_enabled(false);

    eight.set_slow_handler_hook(Duration::from_secs(60), |_| {});
    c.bench_function("publish/8_observers_slow_detection_on", |bench| {
        bench.iter(|| eight.publish(black_box(&Tick(1))));
    });
    eight.clear_slow_handler_hook();

    let interceptors = EventBus::new();
    let _interceptors: Vec<Subscription> = (0..4)
        .map(|_| {
            interceptors.subscribe_mut(|tick: &mut Tick| {
                tick.0 = tick.0.wrapping_add(1);
                Flow::Continue
            })
        })
        .collect();
    c.bench_function("publish_mut/4_interceptors", |bench| {
        let mut tick = Tick(0);
        bench.iter(|| interceptors.publish_mut(black_box(&mut tick)));
    });
}

fn subscribe(c: &mut Criterion) {
    let empty = EventBus::new();
    c.bench_function("subscribe/subscribe_and_drop_on_an_empty_bus", |bench| {
        bench.iter(|| {
            let subscription = empty.subscribe(|_: &Tick| {});
            drop(black_box(subscription));
        });
    });

    let sink = Arc::new(AtomicU64::new(0));
    let busy = EventBus::new();
    let _busy = cheap_observers(&busy, 64, &sink);
    c.bench_function("subscribe/subscribe_and_drop_with_64_handlers", |bench| {
        bench.iter(|| {
            let subscription = busy.subscribe(|_: &Tick| {});
            drop(black_box(subscription));
        });
    });
}

fn queue(c: &mut Criterion) {
    let sink = Arc::new(AtomicU64::new(0));
    let bus = EventBus::new();
    let _observer = cheap_observers(&bus, 1, &sink);

    c.bench_function("queue/post_1000_then_dispatch", |bench| {
        bench.iter(|| {
            for value in 0..1000_u64 {
                bus.post(Tick(black_box(value)));
            }
            bus.dispatch()
        });
    });

    let sender = bus.sender();
    c.bench_function("queue/sender_post_1000_then_dispatch", |bench| {
        bench.iter(|| {
            for value in 0..1000_u64 {
                sender.post(Tick(black_box(value))).unwrap();
            }
            bus.dispatch()
        });
    });
}

fn parallel(c: &mut Criterion) {
    let pool = ThreadPool::new(PoolConfig::new().with_workers(WorkerCount::Exact(4))).unwrap();
    let spawner = pool.spawner();
    let sink = Arc::new(AtomicU64::new(0));

    // Four heavy observers of one event: `publish` runs them one after the other, `publish_parallel` as jobs.
    let bus = EventBus::new();
    let _observers: Vec<Subscription> = (0..4)
        .map(|_| heavy_observer::<Tick>(&bus, &sink))
        .collect();
    c.bench_function("parallel/publish_4_heavy_observers", |bench| {
        bench.iter(|| bus.publish(black_box(&Tick(1))));
    });
    c.bench_function("parallel/publish_parallel_4_heavy_observers", |bench| {
        bench.iter(|| bus.publish_parallel(&spawner, black_box(&Tick(1))));
    });

    // Four event types with a heavy observer each, eight events of each type queued per round.
    let queued = EventBus::new();
    let _a = heavy_observer::<A>(&queued, &sink);
    let _b = heavy_observer::<B>(&queued, &sink);
    let _c = heavy_observer::<C>(&queued, &sink);
    let _d = heavy_observer::<D>(&queued, &sink);
    let post_round = || {
        for _ in 0..8 {
            queued.post(A);
            queued.post(B);
            queued.post(C);
            queued.post(D);
        }
    };
    c.bench_function("parallel/dispatch_4_types_x8", |bench| {
        bench.iter(|| {
            post_round();
            queued.dispatch()
        });
    });
    c.bench_function("parallel/dispatch_parallel_4_types_x8", |bench| {
        bench.iter(|| {
            post_round();
            queued.dispatch_parallel(&spawner)
        });
    });
}

criterion_group!(benches, publish, subscribe, queue, parallel);
criterion_main!(benches);
