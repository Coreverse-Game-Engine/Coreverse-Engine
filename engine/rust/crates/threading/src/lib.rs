//! `threading` — the job system of the Coreverse Engine: a thread pool that runs closures as jobs and gets
//! their results (or panics) back to the code that spawned them.
//!
//! The crate is built in milestones. This is **T6: observation and scratch memory**, on top of T1 (the
//! foundation), T2 (queues, stealing, priorities, spawners), T3 (scoped jobs, parallel loops), T4 (dependencies,
//! task graphs) and T5 (dedicated threads, roles, the IO pool); benchmarks and the rest of the documentation
//! follow (see `docs/src/threading.md`).
//!
//! # What is here today
//!
//! * [`ThreadPool`]: a pool of named worker threads. Every worker has a queue of its own; jobs that come from
//!   outside the pool wait in a shared first-in-first-out queue; idle workers steal from the others. Created
//!   from a [`PoolConfig`] (worker count, thread name prefix, thread priority) and not a global: whoever
//!   creates it owns it. Dropping it, or [`ThreadPool::shutdown`], finishes the queued jobs (and the jobs
//!   they spawn) and joins the workers.
//! * [`ThreadPool::spawn`] / [`ThreadPool::spawn_with_priority`] and [`JobHandle`]: queue a closure, then
//!   [`wait`](JobHandle::wait) for its result. A job that panics does not hurt the pool; the panic comes back
//!   through the handle ([`wait`](JobHandle::wait) re-raises it, [`wait_catch`](JobHandle::wait_catch) returns
//!   a [`JobPanic`]).
//! * [`JobPriority`]: `High`, `Normal` (default) and `Low`. A higher priority always starts before a queued job
//!   of a lower one.
//! * [`Spawner`] ([`ThreadPool::spawner`]): a cloneable handle that spawns jobs without owning the pool; this
//!   is what a job carries to start more jobs. A job spawned from a worker goes into that worker's own queue.
//! * [`worker_index`]: which worker the calling thread is, if any.
//! * [`ThreadPool::scope`] and [`Scope::spawn`]: jobs that **borrow from the caller's stack** (no `Arc`, no
//!   `'static`). The scope waits for all of them, so the borrows are sound; a panic in a job is re-raised
//!   after all jobs finished.
//! * [`ThreadPool::join`]: run two closures, possibly in parallel, and get both results.
//! * [`ThreadPool::parallel_for`], [`parallel_chunks`](ThreadPool::parallel_chunks),
//!   [`parallel_for_each`](ThreadPool::parallel_for_each),
//!   [`parallel_for_each_mut`](ThreadPool::parallel_for_each_mut) and
//!   [`parallel_chunks_mut`](ThreadPool::parallel_chunks_mut): data-parallel loops over index ranges and
//!   slices, with chunk boundaries that do not depend on scheduling. A [`Spawner`] has the same operations,
//!   so a job can parallelize its own work.
//! * [`JobHandle::then`] and [`ThreadPool::when_all`]: dependencies between jobs without blocking anybody. A
//!   continuation is queued by the thread that finishes the job it follows; `when_all` is done when the last
//!   of a group of jobs is. A panic travels along the chain instead of running the continuation.
//! * [`TaskGraph`]: a graph of tasks (closures) with dependencies. A dependency that would close a cycle is
//!   refused when it is added ([`ThreadingError::DependencyCycle`], with the path). [`ThreadPool::run_graph`]
//!   runs it with a counter per task, in parallel, with tasks that may borrow from the caller's stack; a task
//!   that panics makes its dependents skip (the rest still runs) and the first panic is re-raised, or returned
//!   by [`run_graph_catch`](ThreadPool::run_graph_catch) as a [`GraphPanic`].
//! * [`DedicatedThread`]: a named thread with a role, a priority and a controlled lifetime, for work that must
//!   not share a pool (render, audio). The closure gets a [`StopToken`]; dropping the handle stops and joins.
//! * [`ThreadRole`]: which job the calling thread has (`Main`, `Worker`, `Io`, `Render`, `Audio`, `Custom`).
//!   The runtime marks the main thread with [`ThreadRole::Main`]`.enter()`; code that is restricted to one kind
//!   of thread says so with [`ThreadRole::assert_current`] (or [`assert_main_thread`]).
//! * [`PoolConfig::io`]: the preset of an **IO pool** (two workers named `cv-io-N`, role `Io`) so that blocking
//!   work cannot occupy the compute workers. It is an ordinary pool created by the runtime, not a global;
//!   [`JobHandle::then_on`] moves a result from one pool to another.
//! * [`JobObserver`]: hooks for a profiler (a job started or finished, a job was stolen, a worker slept or woke),
//!   installed with [`PoolConfig::with_observer`]. The crate does not depend on any profiler. [`ThreadPool::stats`]
//!   returns always-on counters per worker ([`PoolStats`], [`WorkerStats`]).
//! * Scratch memory: [`PoolConfig::with_scratch`] gives every worker a [`memory::FrameArena`] registered with a
//!   [`memory::MemoryTag`]; jobs reach their thread's arena with [`with_scratch`], and
//!   [`ThreadPool::begin_scratch_frame`] announces a new frame.
//! * Help while waiting: a thread that waits for a job runs queued jobs instead of sleeping, so a job that
//!   waits for another job cannot deadlock a small pool.
//! * [`WorkerCount::Exact(0)`](WorkerCount::Exact): a pool with no threads at all. Jobs then run on the thread
//!   that waits for them or calls [`ThreadPool::run_pending`], which makes tests and single-threaded builds
//!   deterministic.
//! * [`ThreadingError`] and [`ThreadingResult`], convertible into [`ccore::CoreError`].
//!
//! ```
//! use threading::{PoolConfig, ThreadPool};
//!
//! let pool = ThreadPool::new(PoolConfig::new())?;
//! let answer = pool.spawn(|| 6 * 7);
//! assert_eq!(answer.wait(), 42);
//! # Ok::<(), threading::ThreadingError>(())
//! ```
//!
//! # Conventions
//!
//! * `std` only (no `no_std` mode). The crate has exactly one `unsafe` block, in the private `scope` module
//!   (it hides the lifetime of a scoped job from the queues); the module documents the invariants that make
//!   it sound, and the public API is safe. Task graphs and continuations are built from safe code on top of
//!   it.
//! * Jobs started with [`ThreadPool::spawn`] are `Send + 'static`; jobs that borrow from the caller use
//!   [`ThreadPool::scope`].
//! * A `Mutex` of this crate is never held while job code (a job, a continuation or a graph task) runs, so a
//!   job cannot deadlock the pool by what it does inside its closure.
//!
//! The OS-level thread name and priority come from [`platform::thread`]; [`ThreadPriority`] is re-exported
//! from there.

mod config;
mod dedicated;
mod error;
mod graph;
mod job;
mod observe;
mod parallel;
mod pool;
mod priority;
mod queue;
mod role;
#[allow(
    unsafe_code,
    reason = "scoped jobs hide the lifetime of the data they borrow from the pool's queues; the module documents the invariants that make this sound, the single unsafe block carries a safety comment, and the public API is safe"
)]
mod scope;
mod scratch;
mod sleep;
mod sync;
mod worker;

pub use config::{
    DEFAULT_IO_WORKERS, DEFAULT_NAME_PREFIX, IO_NAME_PREFIX, MAX_WORKERS, PoolConfig, WorkerCount,
};
pub use dedicated::{DedicatedThread, StopToken, ThreadConfig};
pub use error::{ThreadingError, ThreadingResult};
pub use graph::{GraphPanic, TaskGraph, TaskId};
pub use job::{JobHandle, JobPanic};
pub use observe::{
    JobFinish, JobObserver, JobStart, JobSteal, PoolId, PoolStats, WorkerSleep, WorkerStats,
    WorkerWake,
};
pub use platform::thread::ThreadPriority;
pub use pool::{Spawner, ThreadPool};
pub use priority::JobPriority;
pub use role::{RoleGuard, ThreadRole, assert_main_thread, is_main_thread};
pub use scope::Scope;
pub use scratch::with_scratch;
pub use worker::worker_index;
