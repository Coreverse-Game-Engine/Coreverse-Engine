# Threading

`engine/rust/crates/threading` is the engine's job system. It is being built in milestones; this page describes what
exists (T1 foundation, T2 queues/priorities/spawners, T3 scoped jobs and parallel loops) and what is planned. The crate
uses `std` only and has one `unsafe` block (see "Scoped jobs" below).

## What is there (T1, T2 and T3)

| Item                                | Purpose                                                                                                                                              |
|:------------------------------------|:-----------------------------------------------------------------------------------------------------------------------------------------------------|
| `ThreadPool`                        | A pool of named worker threads with a queue per worker and a shared queue for outside jobs. `new(config)`, `spawn(closure)`, `spawn_with_priority(priority, closure)`, `spawner()`, `run_pending()`, `shutdown()`, `worker_count()`, `pending_jobs()`. |
| `JobPriority`                       | `Low`, `Normal` (default), `High`. Strict: a higher priority always starts first.                                                                    |
| `Spawner`                           | A cloneable handle that spawns jobs without owning the pool; what a job carries to start more jobs. Fails with `PoolClosed` once the shutdown has begun (unless called from a running job). Also has `scope`, `join` and the `parallel_*` operations. |
| `Scope`, `ThreadPool::scope`        | Scoped jobs: `scope.spawn(...)` may borrow from the surrounding code; `scope` returns when all of them are done.                                      |
| `join`                              | `pool.join(a, b)`: `a` on this thread, `b` as a job; both results back; both may borrow.                                                               |
| `parallel_*`                        | `parallel_for(range, f)`, `parallel_chunks(range, chunk, f)`, `parallel_for_each(slice, f)`, `parallel_for_each_mut(slice, f)`, `parallel_chunks_mut(slice, chunk, f)`. |
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

// Scoped jobs borrow from the stack: no `Arc`, no `'static`.
let mut squares = vec![0_u64; 8];
pool.scope(|scope| {
    for (n, slot) in squares.iter_mut().enumerate() {
        scope.spawn(move || *slot = (n * n) as u64);
    }
});

// Data-parallel loops.
let total = std::sync::atomic::AtomicU64::new(0);
pool.parallel_for(0..1_000, |i| {
    total.fetch_add(i as u64, std::sync::atomic::Ordering::Relaxed);
});

// Dropping the pool (or `pool.shutdown()`) finishes the queued jobs, then joins the workers.
```

## Behaviour worth knowing

* **No global pool.** The code that creates a `ThreadPool` owns it and hands out `&ThreadPool` or an
  `Arc<ThreadPool>`. Nothing is initialized behind the program's back.
* **Worker count.** `WorkerCount::Auto` is `max(1, logical cores - 1)` (the logical cores come from
  `std::thread::available_parallelism`, the same call `platform::info::cpu_info()` makes, without its physical-core
  query): the thread that creates the pool (the game loop, the editor's GUI thread) keeps a core. `Exact(n)` is limited to `MAX_WORKERS` (1024). Workers are named
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
  last owner of its own pool. Once the pool's shutdown has begun, `Spawner::spawn` returns `PoolClosed` for every
  thread that is not running one of the pool's jobs; a thread that is running one (a worker, or a thread that helps
  while it waits, or the thread that is shutting the pool down) can still spawn, because the jobs a running job
  spawns are part of the work the shutdown has to finish. Every job a spawner got accepted runs, even when spawning
  races with the shutdown (the shared queue closes only when it is seen empty under its lock).
* **The shutdown is finite.** It ends after the jobs that were queued when it began, and whatever those jobs spawn,
  have run, whatever other threads do. (An earlier version accepted jobs from any thread until it saw the queue
  empty; a thread that kept spawning jobs a little faster than the workers ran them could keep the queue non-empty
  for ever, so `drop(pool)` never returned and the producer, which only stops once the pool is closed, never
  stopped. The regression test is `a_pool_shuts_down_even_while_another_thread_keeps_spawning`.) A job that never
  ends, or that spawns jobs for ever, still keeps the shutdown from ending: that is the job's doing.
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

## Scoped jobs, `join` and the parallel loops (T3)

* **`scope`.** `pool.scope(|scope| { scope.spawn(|| ...); })` runs jobs that may borrow anything that outlives the
  call. It returns the body's value, but only after every job spawned in it has run to the end and been dropped.
  While it waits, the calling thread runs queued jobs itself (so it works with no workers). A job may spawn more
  jobs through the `&Scope` it captured. `scope.spawn` returns nothing: hand results back through what the job
  borrows (a `&mut` element, an atomic, a `Mutex`) or use `join`.
* **Panics.** A panicking job does not stop the others. When all are done, the scope re-raises the first job panic.
  If the body itself panics, the jobs are still waited for first and the body's panic wins. Nothing is cancelled.
* **`join(a, b)`.** `a` runs on the calling thread, `b` as a scoped job. Because a job spawned by a job on a worker
  goes into that worker's own queue and the worker takes its newest job back first, an unstolen `b` is simply run by
  the caller after `a`: a `join` that nobody helps costs little more than a function call and an allocation.
* **Loops.** `parallel_for`, `parallel_for_each` and `parallel_for_each_mut` cut the work into about four chunks per
  thread. `parallel_chunks` and `parallel_chunks_mut` use the chunk size you give, with the boundaries of
  `slice::chunks` (the last chunk may be shorter), no matter how the work is scheduled; `parallel_chunks_mut` passes the
  index of the chunk's first item. Chunks are handed out by splitting in halves with `join`, so the order of the calls
  is unspecified. Slices are split with `split_at(_mut)`, so no two jobs ever share an item (this part is safe code).
  Choose a chunk size so that one chunk is worth a job (some microseconds of work). A chunk size of 0 panics.
* **From inside a job.** Use the job's `Spawner`: it has the same `scope`, `join` and `parallel_*` operations. They
  also work after the pool has shut down, then on the calling thread alone.

### Why the `unsafe` block is sound

The queues hold `'static` jobs, so a scoped job's borrowed lifetime has to be hidden from them (`mem::transmute` of
`Box<dyn FnOnce() + Send + 'a>` to `'static`, in `scope.rs`: the only `unsafe` in the crate). That is sound because
the scope never lets the borrowed data go while a job can still touch it:

1. every job owns a guard that counts it as pending until the job is dropped;
2. the job is declared before the guard in its struct, so the job and everything it captured are dropped first;
3. `scope` waits for the count to reach 0 before it returns or unwinds (the body runs inside `catch_unwind`; if the
   wait itself ever unwound, an abort guard stops the process instead of freeing the stack frame under the jobs);
4. the counter lives under a mutex, so the jobs' writes are visible to the caller when it returns;
5. `'scope` is invariant and higher-ranked, as in `std::thread::scope`, so a `&Scope` cannot escape and a job can only
   borrow data that outlives the whole call.

The compiler rejects the misuse cases (a job borrowing something that dies with the body, a `&Scope` escaping,
two jobs borrowing the same variable mutably, reading what a job may change, capturing a non-`Send` value, using a
value after moving it into a job); each is a `compile_fail` doc test on `Scope`. Miri is the tool that checks the one
`unsafe` block at run time; see "Running Miri" below.

## Running Miri

Miri checks the `unsafe` block in `scope.rs` (and everything else the tests execute) for undefined behaviour. It needs
a nightly toolchain: `rustup +nightly component add miri`. It runs the code in an interpreter, hundreds of times
slower than native, so run the test targets one by one:

```text
cargo +nightly miri test -p threading --lib
cargo +nightly miri test -p threading --test pool
cargo +nightly miri test -p threading --test scheduling
cargo +nightly miri test -p threading --test scoped
cargo +nightly miri test -p threading --doc
```

* The tests scale their workload down under Miri (`scaled(normal, under_miri)` in each test file; the behaviour tested is
  the same, only the amount of work changes).
* Two tests are ignored under Miri (`#[cfg_attr(miri, ignore)]`): the ones that ask the OS for a worker thread priority,
  because Miri cannot call `SetThreadPriority` and the like. That call is `platform`'s FFI, there is nothing for Miri to
  check in it.
* `WorkerCount::Auto` does not call `platform::info::cpu_info()` (which also runs `GetLogicalProcessorInformationEx` on
  Windows, an OS call Miri cannot execute).
* Threads interleave differently with every seed. To explore more interleavings, run a target with several seeds, for
  example in PowerShell `$env:MIRIFLAGS = "-Zmiri-many-seeds=0..8"` before the command (it runs the whole target once
  per seed, so it multiplies the time). Add `-Zmiri-backtrace=full` to get complete backtraces when something fails.
* Miri finds undefined behaviour (invalid pointers, lifetimes, aliasing, data races on non-atomic data) in the paths the
  tests execute. It does not find logic races or deadlocks, which is what the repeated native runs are for.

## Roadmap

| Milestone | Content                                                                                                                                             |
|:----------|:----------------------------------------------------------------------------------------------------------------------------------------------------|
| T1        | Done: pool, `spawn`, `JobHandle`, panic propagation, shutdown, single-threaded mode, help while waiting.                                            |
| T2        | Done: per-worker queues with stealing, three priorities (`High`/`Normal`/`Low`), `Spawner` for jobs that spawn jobs, `worker_index()`.              |
| T3        | Done: `scope`, `join`, `parallel_for`, `parallel_chunks`, `parallel_for_each` (and `_mut` variants): jobs that borrow from the stack. The first `unsafe` module of the crate. |
| T4        | Dependencies between jobs (`then`, counters) and a `TaskGraph` with cycle detection, for the ECS scheduler.                                         |
| T5        | Dedicated threads (render, audio), a separate IO pool for blocking work, a main-thread marker and `ThreadRole` assertions.                          |
| T6        | Observer hooks for the profiler, per-worker statistics, per-worker scratch arenas on top of `memory`.                                               |
| T7        | criterion benchmarks, the rest of the documentation, an optional Miri/loom job (the `scope` module is what it is for).                              |

Not planned for now: an `async` executor, fibers, `no_std`, an FFI for C++ (it comes when C++ needs to submit jobs,
for example for a render thread), a lock-free queue (only if measurements ask for it). The queues live in one module (`queue.rs`) behind a small
interface (`push_external`, `push_local`, `pop`, `close_if_empty`), so a lock-free version can replace it later.
