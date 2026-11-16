# Threading

`engine/rust/crates/threading` is the engine's job system. It is being built in milestones; this page describes what
exists (T1, the foundation) and what is planned. The crate uses `std` only and, in T1, no `unsafe` code.

## What is there (T1)

| Item                                | Purpose                                                                                                                                              |
|:------------------------------------|:-----------------------------------------------------------------------------------------------------------------------------------------------------|
| `ThreadPool`                        | A pool of named worker threads that share one first-in-first-out queue. `new(config)`, `spawn(closure)`, `run_pending()`, `shutdown()`, `worker_count()`, `pending_jobs()`. |
| `PoolConfig`, `WorkerCount`         | The settings: worker count (`Auto` or `Exact(n)`), thread name prefix, thread priority. Chained `with_*` setters, `validate()`.                       |
| `JobHandle<T>`                      | What `spawn` returns: `is_done()`, `wait()` (re-raises a panic), `wait_catch()` (returns the panic as a value).                                      |
| `JobPanic`                          | A job's panic as a value: `message()`, `into_payload()`, `Display`, `Error`.                                                                          |
| `ThreadingError`, `ThreadingResult` | `InvalidConfig` and `Spawn`. `#[non_exhaustive]`, convertible into `ccore::CoreError`.                                                                |
| `ThreadPriority`                    | Re-exported from `platform::thread`.                                                                                                                  |

```rust,ignore
use threading::{PoolConfig, ThreadPool, WorkerCount};

// A pool is an ordinary value: whoever creates it owns it.
let pool = ThreadPool::new(PoolConfig::new())?; // Auto: logical cores - 1, at least 1

let answer = pool.spawn(|| 6 * 7);
assert_eq!(answer.wait(), 42);

// A job that panics does not hurt the pool; the panic comes back through the handle.
let failed = pool.spawn(|| -> u32 { panic!("boom") });
assert_eq!(failed.wait_catch().unwrap_err().message(), Some("boom"));

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
| T2        | Per-worker queues with stealing, three priorities (`High`/`Normal`/`Low`), a way for jobs to spawn jobs without holding the pool.                   |
| T3        | `scope`, `join`, `parallel_for`, `parallel_chunks`, `parallel_for_each`: jobs that borrow from the stack. The first `unsafe` module of the crate.   |
| T4        | Dependencies between jobs (`then`, counters) and a `TaskGraph` with cycle detection, for the ECS scheduler.                                         |
| T5        | Dedicated threads (render, audio), a separate IO pool for blocking work, a main-thread marker and `ThreadRole` assertions.                          |
| T6        | Observer hooks for the profiler, per-worker statistics, per-worker scratch arenas on top of `memory`.                                               |
| T7        | criterion benchmarks, the rest of the documentation, optional Miri/loom job.                                                                        |

Not planned for now: an `async` executor, fibers, `no_std`, an FFI for C++ (it comes when C++ needs to submit jobs,
for example for a render thread), a lock-free queue (only if measurements ask for it).
