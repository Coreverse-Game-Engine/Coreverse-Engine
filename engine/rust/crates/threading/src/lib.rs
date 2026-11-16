//! `threading` — the job system of the Coreverse Engine: a thread pool that runs closures as jobs and gets
//! their results (or panics) back to the code that spawned them.
//!
//! The crate is built in milestones. This is **T2: per-worker queues with stealing, job priorities and
//! spawners**, on top of the T1 foundation; scoped jobs, parallel loops, dependencies and dedicated threads
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
//! * No `unsafe` code, and `std` only (no `no_std` mode).
//! * Job closures are `Send + 'static` in this milestone. Jobs that borrow from the caller's stack arrive with
//!   scoped jobs.
//! * A `Mutex` of this crate is never held while job code runs, so a job cannot deadlock the pool by what it
//!   does inside its closure.
//!
//! The OS-level thread name and priority come from [`platform::thread`]; [`ThreadPriority`] is re-exported
//! from there.

mod config;
mod error;
mod job;
mod pool;
mod priority;
mod queue;
mod sleep;
mod sync;
mod worker;

pub use config::{DEFAULT_NAME_PREFIX, MAX_WORKERS, PoolConfig, WorkerCount};
pub use error::{ThreadingError, ThreadingResult};
pub use job::{JobHandle, JobPanic};
pub use platform::thread::ThreadPriority;
pub use pool::{Spawner, ThreadPool};
pub use priority::JobPriority;
pub use worker::worker_index;
