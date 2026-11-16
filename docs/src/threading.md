# Threading

`engine/rust/crates/threading` is the engine's job system. It is being built in milestones; this page describes what
exists (T1, the foundation, and T2, queues, priorities and spawners) and what is planned. The crate uses `std` only and,
so far, no `unsafe` code.

## What is there (T1 and T2)

| Item                                | Purpose                                                                                                                                              |
|:------------------------------------|:-----------------------------------------------------------------------------------------------------------------------------------------------------|
| `ThreadPool`                        | A pool of named worker threads with a queue per worker and a shared queue for outside jobs. `new(config)`, `spawn(closure)`, `spawn_with_priority(priority, closure)`, `spawner()`, `run_pending()`, `shutdown()`, `worker_count()`, `pending_jobs()`. |
| `JobPriority`                       | `Low`, `Normal` (default), `High`. Strict: a higher priority always starts first.                                                                    |
| `Spawner`                           | A cloneable handle that spawns jobs without owning the pool; what a job carries to start more jobs. Fails with `PoolClosed` after shutdown.          |
| `worker_index()`                    | The index of the calling thread among its pool's workers, or `None` on any other thread.                                                              |
| `PoolConfig`, `WorkerCount`         | The settings: worker count (`Auto` or `Exact(n)`), thread name prefix, thread priority. Chained `with_*` setters, `validate()`.                       |
| `JobHandle<T>`                      | What `spawn` returns: `is_done()`, `wait()` (re-raises a panic), `wait_catch()` (returns the panic as a value).                                      |
| `JobPanic`                          | A job's panic as a value: `message()`, `into_payload()`, `Display`, `Error`.                                                                          |
| `ThreadingError`, `ThreadingResult` | `InvalidConfig`, `Spawn` and `PoolClosed`. `#[non_exhaustive]`, convertible into `ccore::CoreError`.                                                                |
| `ThreadPriority`                    | Re-exported from `platform::thread`.                                                                                                                  |

```rust,ignore
use threading::{JobPriority, PoolConfig, ThreadPool, ThreadingError};

// A pool is an ordinary value: whoever creates it owns it.
let pool = ThreadPool::new(PoolConfig::new())?; // Auto: logical cores - 1, at least 1

let answer = pool.spawn(|| 6 * 7);
assert_eq!(answer.wait(), 42);

// A job that panics does not hurt the pool; the panic comes back through the handle.
let failed = pool.spawn(|| -> u32 { panic!("boom") });
assert_eq!(failed.wait_catch().unwrap_err().message(), Some("boom"));

// Priorities, and jobs that start jobs: a job carries a `Spawner`, not the pool.
let spawner = pool.spawner();
let report = pool.spawn_with_priority(JobPriority::High, move || {
    let part = spawner.spawn(|| 20)?; // goes into this worker's own queue
    Ok::<_, ThreadingError>(part.wait() + 1)
});
assert_eq!(report.wait()?, 21);

// Dropping the pool (or `pool.shutdown()`) finishes the queued jobs, then joins the workers.
```

## Behaviour worth knowing

* **No global pool.** The code that creates a `ThreadPool` owns it and hands out `&ThreadPool` or an
  `Arc<ThreadPool>`. Nothing is initialized behind the program's back.
* **Worker count.** `WorkerCount::Auto` is `max(1, logical cores - 1)`: the thread that creates the pool (the game
  loop, the editor's GUI thread) keeps a core. `Exact(n)` is limited to `MAX_WORKERS` (1024). Workers are named
  `<prefix>-<index>` (default prefix `cv-worker`); the OS may shorten the name (Linux keeps 15 bytes).
* **Thread priority** is a request. A worker asks the OS for the configured priority when it starts; if the OS
  refuses (on Linux, raising a priority needs a privilege), the worker keeps the default and works as usual.
* **Panics.** A job runs inside `catch_unwind`, so a panic never takes a worker down. `wait` re-raises the original
  panic on the waiting thread; `wait_catch` returns it as a `JobPanic`. If the handle was dropped, the payload is
  discarded; the panic message is still printed by the panic hook, like any panic.
* **Dropping a handle does not cancel the job.** Fire-and-forget works; the result is thrown away.
* **Shutdown finishes the queue.** Dropping the pool, or `shutdown()`, runs every queued job first. A `JobHandle` may
  outlive the pool and never waits for a job that will not happen. If the last owner of the pool is a job on one of
  its own workers, that worker does not try to join itself; it leaves when the job returns.
* **Queues and stealing.** Every worker has a queue of its own. Jobs spawned from a thread that is not one of the
  pool's workers go into a shared queue; a job spawned by a job on a worker goes into that worker's queue. A worker
  looks for work in this order: its own queue, the shared queue, then the other workers' queues. It takes its own
  newest job first (the one most likely to be a child of what it just did, and the one that keeps a waiting job's
  help shallow); it steals the oldest job of another worker; and it takes shared jobs in the order they were spawned.
* **Priorities are strict.** The search above is done for `High` first, then `Normal`, then `Low`, over *all* queues.
  So a `High` job in another worker's queue is stolen before a `Normal` job in the worker's own queue. A running
  job is never interrupted, and a steady stream of `High` jobs can starve `Low` ones; use `High` only for short,
  latency-sensitive work.
* **Order guarantees.** Higher priority starts first. Within one priority, jobs spawned from outside the pool start
  in spawn order. For jobs spawned by jobs nothing more is promised than the newest-first / oldest-stolen rule, and
  with more than one worker nothing is promised about the order in which jobs *finish*. A test that logs from jobs
  must not let a waiting thread run jobs concurrently with the worker (see "help while waiting").
* **Spawners.** A `Spawner` does not keep the pool alive, unlike an `Arc<ThreadPool>`, which could make a job the
  last owner of its own pool. While the pool drains at shutdown, jobs spawned by running jobs are accepted and run;
  once the pool has finished shutting down, `Spawner::spawn` returns `PoolClosed`. Every job a spawner got
  accepted runs, even when spawning races with the shutdown (the shared queue closes only when it is seen empty
  under its lock).
* **Sleeping workers.** An idle worker sleeps on a condition variable. Whoever queues a job raises a per-priority
  counter first, then wakes a sleeper if one exists; a worker that is about to sleep announces itself and re-checks
  the counter under the lock, so a wake-up cannot be lost. The fast path (nobody asleep) is one atomic load.
* **Help while waiting.** A thread that waits for a job does not sleep while jobs are queued: it runs queued jobs
  until its own is finished. A job that waits for another job therefore cannot deadlock a pool with one worker. The
  price is that the waiting thread may run an unrelated job, and it may run the very job it waits for itself, so a
  job cannot assume it runs on a worker thread.
* **Single-threaded mode.** `WorkerCount::Exact(0)` starts no thread. Jobs then run on the thread that waits for them,
  or on the one that calls `run_pending()`, in submission order; dropping the pool runs what is left. Tests and
  single-threaded builds use it because it is deterministic.
* **No lock is held while job code runs.** The crate's mutexes only guard a few lines of its own queue and slot
  code, so what a job does inside its closure cannot deadlock the pool through them. Poisoned locks are taken back
  instead of propagating a panic.
* **Closures are `Send + 'static`.** Jobs that borrow from the caller's stack need scoped jobs (T3).

## Roadmap

| Milestone | Content                                                                                                                                             |
|:----------|:----------------------------------------------------------------------------------------------------------------------------------------------------|
| T1        | Done: pool, `spawn`, `JobHandle`, panic propagation, shutdown, single-threaded mode, help while waiting.                                            |
| T2        | Done: per-worker queues with stealing, three priorities (`High`/`Normal`/`Low`), `Spawner` for jobs that spawn jobs, `worker_index()`.              |
| T3        | `scope`, `join`, `parallel_for`, `parallel_chunks`, `parallel_for_each`: jobs that borrow from the stack. The first `unsafe` module of the crate.   |
| T4        | Dependencies between jobs (`then`, counters) and a `TaskGraph` with cycle detection, for the ECS scheduler.                                         |
| T5        | Dedicated threads (render, audio), a separate IO pool for blocking work, a main-thread marker and `ThreadRole` assertions.                          |
| T6        | Observer hooks for the profiler, per-worker statistics, per-worker scratch arenas on top of `memory`.                                               |
| T7        | criterion benchmarks, the rest of the documentation, optional Miri/loom job.                                                                        |

Not planned for now: an `async` executor, fibers, `no_std`, an FFI for C++ (it comes when C++ needs to submit jobs,
for example for a render thread), a lock-free queue (only if measurements ask for it). The queues live in one module (`queue.rs`) behind a small
interface (`push_external`, `push_local`, `pop`, `close_if_empty`), so a lock-free version can replace it later.
