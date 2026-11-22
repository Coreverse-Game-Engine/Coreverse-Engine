//! Criterion benchmarks for the `threading` crate: what a job costs, how `parallel_for` scales with the worker
//! count, how the queues behave under stealing and under many producers, what dependencies and task graphs cost,
//! and what the observability and scratch features cost.
//!
//! Run with `cargo bench -p threading` (everything takes a few minutes) or pick a group, for example
//! `cargo bench -p threading -- parallel_for`. Add `--quick` for a fast, rough pass.
//!
//! How to read the numbers:
//!
//! * They depend on the machine, the OS scheduler and what else runs; read them as **ratios between benchmarks of
//!   one run**, not as absolute figures. Worker counts are derived from `available_parallelism` (1, 2, 4, ... up to
//!   the core count), so the groups have as many rows as the machine has doublings. On a one-core machine the
//!   scaling groups have a single row and say nothing about scaling.
//! * `no_workers` means a pool in single-threaded mode: the benchmark thread runs the jobs itself, so the figure is
//!   the cost of the queue, the handle and the bookkeeping with no thread hand-off in it.
//! * Every input goes through `black_box` (or the result is returned to criterion) so the optimizer cannot fold
//!   the work away.
#![allow(missing_docs)]

use std::hint::black_box;
use std::ops::Range;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use threading::{
    JobHandle, JobObserver, PoolConfig, TaskGraph, ThreadPool, WorkerCount, with_scratch,
};

/// How many jobs one iteration of the throughput benchmarks submits.
const JOBS: usize = 1000;

/// How many items the `parallel_for` benchmarks process in one iteration (about a million).
const ITEMS: usize = 1 << 20;

fn config(workers: usize) -> PoolConfig {
    PoolConfig::new().with_workers(WorkerCount::Exact(workers))
}

fn pool(workers: usize) -> ThreadPool {
    ThreadPool::new(config(workers)).expect("the pool starts")
}

/// 1, 2, 4, ... up to the number of logical cores, and the core count itself.
fn worker_counts() -> Vec<usize> {
    let cores = thread::available_parallelism().map_or(1, usize::from);
    let mut counts = vec![1];
    let mut next = 2;
    while next < cores {
        counts.push(next);
        next *= 2;
    }
    if cores > 1 {
        counts.push(cores);
    }
    counts
}

/// An observer that overrides nothing: every hook is called and does nothing, so the figure is the cost of
/// being observed (the clock reads, the event structs, the dynamic calls), not of any recording.
struct NoopObserver;

impl JobObserver for NoopObserver {}

/// About 40-60 ns of integer work that cannot be optimized away.
fn mix(seed: usize) -> u64 {
    let mut x = seed as u64 ^ 0x9e37_79b9_7f4a_7c15;
    for _ in 0..32 {
        x = x
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        x ^= x >> 29;
    }
    x
}

/// Spawns `count` empty jobs and waits for all of them.
fn batch(pool: &ThreadPool, count: usize) -> usize {
    let handles: Vec<JobHandle<usize>> = (0..count)
        .map(|index| pool.spawn(move || black_box(index)))
        .collect();
    handles.into_iter().map(JobHandle::wait).sum()
}

/// Runs `count` copies of `job` and returns when all of them are done, **without the calling thread running
/// any of them** (it sleeps on a channel; a thread that waits for a handle would help). Needs workers.
fn run_on_workers<F>(pool: &ThreadPool, count: usize, job: &Arc<F>)
where
    F: Fn() + Send + Sync + 'static,
{
    let remaining = Arc::new(AtomicUsize::new(count));
    let (sender, receiver) = mpsc::channel();
    for _ in 0..count {
        let job = Arc::clone(job);
        let remaining = Arc::clone(&remaining);
        let sender = sender.clone();
        drop(pool.spawn(move || {
            job();
            if remaining.fetch_sub(1, Ordering::AcqRel) == 1 {
                sender.send(()).expect("the benchmark is waiting");
            }
        }));
    }
    receiver.recv().expect("the last job answers");
}

/// What one job costs: the queue, the handle, the allocation, and the optional bookkeeping.
fn job_overhead(c: &mut Criterion) {
    let mut group = c.benchmark_group("job_overhead");

    group.throughput(Throughput::Elements(1));
    let single = pool(0);
    group.bench_function("spawn_and_wait/no_workers", |bench| {
        bench.iter(|| single.spawn(|| black_box(1_u32)).wait());
    });
    // The baseline that makes a pool worth having: a thread per job.
    group.bench_function("std_thread_spawn_and_join", |bench| {
        bench.iter(|| {
            thread::spawn(|| black_box(1_u32))
                .join()
                .expect("the thread ends")
        });
    });

    group.throughput(Throughput::Elements(JOBS as u64));
    // The same batch with the statistics only, with the clock on, and with an observer installed.
    let variants = [
        ("plain", config(0)),
        ("timing", config(0).with_timing(true)),
        ("observer", config(0).with_observer(Arc::new(NoopObserver))),
    ];
    for (name, config) in variants {
        let pool = ThreadPool::new(config).expect("the pool starts");
        group.bench_function(format!("batch_{JOBS}/no_workers_{name}"), |bench| {
            bench.iter(|| batch(&pool, JOBS));
        });
    }
    for workers in worker_counts() {
        let pool = pool(workers);
        group.bench_with_input(
            BenchmarkId::new(format!("batch_{JOBS}/workers"), workers),
            &workers,
            |bench, _| bench.iter(|| batch(&pool, JOBS)),
        );
    }
    group.finish();
}

/// `parallel_for` and friends against the plain loop, for a body that is worth parallelizing, then the same
/// loop with a body so small that the scheduling is all there is, for several chunk sizes.
fn parallel_for_scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("parallel_for");
    group.throughput(Throughput::Elements(ITEMS as u64));

    group.bench_function("serial_loop", |bench| {
        bench.iter(|| {
            let mut acc = 0_u64;
            for index in 0..ITEMS {
                acc ^= mix(black_box(index));
            }
            acc
        });
    });

    let mut counts = vec![0];
    counts.extend(worker_counts());
    for &workers in &counts {
        let pool = pool(workers);

        group.bench_with_input(
            BenchmarkId::new("chunks_4096/workers", workers),
            &workers,
            |bench, _| {
                bench.iter(|| {
                    let total = AtomicU64::new(0);
                    pool.parallel_chunks(0..ITEMS, 4096, |range: Range<usize>| {
                        let mut acc = 0_u64;
                        for index in range {
                            acc ^= mix(black_box(index));
                        }
                        total.fetch_xor(acc, Ordering::Relaxed);
                    });
                    total.load(Ordering::Relaxed)
                });
            },
        );

        let mut items: Vec<u64> = (0..ITEMS as u64).collect();
        group.bench_with_input(
            BenchmarkId::new("for_each_mut/workers", workers),
            &workers,
            |bench, _| {
                bench.iter(|| {
                    pool.parallel_for_each_mut(&mut items, |slot| {
                        *slot = mix(black_box(*slot as usize));
                    });
                    items[ITEMS - 1]
                });
            },
        );
    }
    group.finish();

    // Granularity: the body is one addition, so what is measured is how much a chunk costs to schedule.
    let mut group = c.benchmark_group("parallel_for_granularity");
    group.throughput(Throughput::Elements(ITEMS as u64));
    let workers = *worker_counts().last().expect("at least one count");
    let pool = pool(workers);
    for chunk in [16_usize, 256, 4096, 65_536] {
        group.bench_with_input(
            BenchmarkId::new(format!("tiny_body/workers_{workers}/chunk"), chunk),
            &chunk,
            |bench, &chunk| {
                bench.iter(|| {
                    let total = AtomicU64::new(0);
                    pool.parallel_chunks(0..ITEMS, chunk, |range: Range<usize>| {
                        let mut acc = 0_u64;
                        for index in range {
                            acc = acc.wrapping_add(black_box(index as u64));
                        }
                        total.fetch_add(acc, Ordering::Relaxed);
                    });
                    total.load(Ordering::Relaxed)
                });
            },
        );
    }
    group.finish();
}

/// The queues under pressure: all work born on one worker (the others have to steal it), and many producers
/// outside the pool pushing into the shared queue at the same time.
fn queue_contention(c: &mut Criterion) {
    const FAN_OUT: usize = 10_000;
    const PRODUCERS: usize = 4;
    let mut group = c.benchmark_group("queue_contention");
    group.throughput(Throughput::Elements(FAN_OUT as u64));

    for workers in worker_counts() {
        let pool = pool(workers);
        let spawner = pool.spawner();
        group.bench_with_input(
            BenchmarkId::new("fan_out_from_one_worker/workers", workers),
            &workers,
            |bench, _| {
                bench.iter(|| {
                    let remaining = Arc::new(AtomicUsize::new(FAN_OUT));
                    let (sender, receiver) = mpsc::channel();
                    let spawner = spawner.clone();
                    // One job queues all the others into its own worker's queue; idle workers steal them.
                    drop(pool.spawn(move || {
                        for _ in 0..FAN_OUT {
                            let remaining = Arc::clone(&remaining);
                            let sender = sender.clone();
                            drop(spawner.spawn(move || {
                                black_box(());
                                if remaining.fetch_sub(1, Ordering::AcqRel) == 1 {
                                    sender.send(()).expect("the benchmark is waiting");
                                }
                            }));
                        }
                    }));
                    receiver.recv().expect("the last job answers");
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new(format!("{PRODUCERS}_producers_outside/workers"), workers),
            &workers,
            |bench, _| {
                bench.iter(|| {
                    let remaining = Arc::new(AtomicUsize::new(FAN_OUT));
                    let (sender, receiver) = mpsc::channel();
                    thread::scope(|scope| {
                        for _ in 0..PRODUCERS {
                            let spawner = spawner.clone();
                            let remaining = Arc::clone(&remaining);
                            let sender = sender.clone();
                            scope.spawn(move || {
                                for _ in 0..FAN_OUT / PRODUCERS {
                                    let remaining = Arc::clone(&remaining);
                                    let sender = sender.clone();
                                    drop(spawner.spawn(move || {
                                        black_box(());
                                        if remaining.fetch_sub(1, Ordering::AcqRel) == 1 {
                                            sender.send(()).expect("the benchmark is waiting");
                                        }
                                    }));
                                }
                            });
                        }
                    });
                    receiver.recv().expect("the last job answers");
                });
            },
        );
    }
    group.finish();
}

/// Continuations and task graphs: what a dependency costs per task.
fn dependencies(c: &mut Criterion) {
    let mut group = c.benchmark_group("dependencies");
    group.throughput(Throughput::Elements(JOBS as u64));

    let mut counts = vec![0];
    counts.push(*worker_counts().last().expect("at least one count"));
    counts.dedup();
    for &workers in &counts {
        let pool = pool(workers);

        group.bench_with_input(
            BenchmarkId::new(format!("then_chain_{JOBS}/workers"), workers),
            &workers,
            |bench, _| {
                bench.iter(|| {
                    let mut handle = pool.spawn(|| 0_usize);
                    for _ in 0..JOBS {
                        handle = handle.then(|n| black_box(n) + 1);
                    }
                    handle.wait()
                });
            },
        );

        // A chain: every task waits for the one before it, so nothing runs in parallel.
        let mut chain = TaskGraph::with_capacity(JOBS);
        let mut previous = chain.add("link", || {
            black_box(());
        });
        for _ in 1..JOBS {
            previous = chain
                .add_after("link", &[previous], || {
                    black_box(());
                })
                .expect("the id is from this graph");
        }
        group.bench_with_input(
            BenchmarkId::new(format!("graph_chain_{JOBS}/workers"), workers),
            &workers,
            |bench, _| bench.iter(|| pool.run_graph(&mut chain)),
        );

        // Ten layers of a hundred tasks; each task waits for two tasks of the layer before.
        let mut layered = TaskGraph::with_capacity(JOBS);
        let mut layer: Vec<_> = (0..100)
            .map(|_| {
                layered.add("task", || {
                    black_box(());
                })
            })
            .collect();
        for _ in 1..10 {
            let mut next = Vec::with_capacity(100);
            for index in 0..100 {
                let after = [layer[index], layer[(index + 1) % 100]];
                next.push(
                    layered
                        .add_after("task", &after, || {
                            black_box(());
                        })
                        .expect("the ids are from this graph"),
                );
            }
            layer = next;
        }
        group.bench_with_input(
            BenchmarkId::new("graph_layers_10x100/workers", workers),
            &workers,
            |bench, _| bench.iter(|| pool.run_graph(&mut layered)),
        );
    }
    group.finish();
}

/// Scratch memory against the heap for the temporary data of a job, and the cost of reaching the arena.
fn scratch_memory(c: &mut Criterion) {
    const FLOATS: usize = 256;
    let workers = *worker_counts().last().expect("at least one count");
    let mut group = c.benchmark_group("scratch_memory");
    group.throughput(Throughput::Elements(JOBS as u64));

    let with_arenas =
        ThreadPool::new(config(workers).with_scratch(2 * 1024 * 1024)).expect("the pool starts");
    let scratch_job = Arc::new(|| {
        let last = with_scratch(|arena| {
            let values = arena
                .alloc_slice_fill(FLOATS, black_box(1.0_f32))
                .expect("room in the arena");
            values[FLOATS - 1]
        })
        .unwrap_or(0.0);
        black_box(last);
    });
    group.bench_function(
        format!("scratch_{FLOATS}_f32_x{JOBS}/workers_{workers}"),
        |bench| {
            bench.iter(|| {
                with_arenas.begin_scratch_frame();
                run_on_workers(&with_arenas, JOBS, &scratch_job);
            });
        },
    );

    let plain = pool(workers);
    let heap_job = Arc::new(|| {
        let values = vec![black_box(1.0_f32); FLOATS];
        black_box(values[FLOATS - 1]);
    });
    group.bench_function(
        format!("heap_{FLOATS}_f32_x{JOBS}/workers_{workers}"),
        |bench| bench.iter(|| run_on_workers(&plain, JOBS, &heap_job)),
    );

    // The price of `with_scratch` itself (a thread-local lookup and an `Rc` clone), measured inside one job on a
    // worker, because that is the only place where it has an arena.
    group.throughput(Throughput::Elements(1));
    group.bench_function("with_scratch_access/on_a_worker", |bench| {
        bench.iter_custom(|iterations| {
            let (sender, receiver) = mpsc::channel();
            drop(with_arenas.spawn(move || {
                let started = Instant::now();
                for _ in 0..iterations {
                    black_box(with_scratch(|_| black_box(())));
                }
                sender
                    .send(started.elapsed())
                    .expect("the benchmark is waiting");
            }));
            receiver.recv().expect("the job answers")
        });
    });
    group.finish();
}

/// The statistics are always on; what does reading them cost?
fn statistics(c: &mut Criterion) {
    let mut group = c.benchmark_group("statistics");
    let workers = *worker_counts().last().expect("at least one count");
    let pool = pool(workers);
    batch(&pool, 100);
    let sink = Mutex::new(0_u64);
    group.bench_function(format!("snapshot/workers_{workers}"), |bench| {
        bench.iter(|| {
            let stats = pool.stats();
            *sink.lock().expect("not poisoned") = stats.total().jobs_run;
            stats
        });
    });
    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .warm_up_time(Duration::from_millis(500))
        .measurement_time(Duration::from_secs(2))
        .sample_size(20);
    targets = job_overhead, parallel_for_scaling, queue_contention, dependencies, scratch_memory, statistics
}
criterion_main!(benches);
