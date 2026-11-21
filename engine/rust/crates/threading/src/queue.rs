//! The job queues: one shared queue for jobs that come from outside the pool, one queue per worker for jobs
//! that workers spawn, and the rules for taking from them.
//!
//! This is the only module that knows how jobs are stored. The rest of the crate pushes and pops through
//! [`Queues`], so a different (lock-free) implementation can replace it without touching the pool.
//!
//! # Rules
//!
//! * **Strict priority first.** Every search goes through the priority levels from `High` to `Low`; within a
//!   level it looks in the worker's own queue, then in the shared queue, then in the other workers' queues.
//! * **Own queue: newest first** (`pop_back`). A job that spawns children usually wants to finish them itself
//!   while their data is still warm, and running the newest keeps a waiting job's help shallow.
//! * **Shared queue: oldest first** (`pop_front`), so outside callers see first-in-first-out within a priority.
//! * **Stealing: oldest first** (`pop_front` of the victim's queue), so a thief takes the work that has waited
//!   longest and rarely the work its owner is about to run.
//!
//! Every queue has one lane per priority behind a single mutex. Locks are held only for the push or pop itself.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::priority::{JobPriority, LANES};
use crate::sync::lock;

/// One queued unit of work. Everything a job needs (its closure, the slot for its result) is inside.
pub(crate) type Task = Box<dyn FnOnce() + Send + 'static>;

type Lanes = [VecDeque<Task>; LANES];

fn empty_lanes() -> Lanes {
    std::array::from_fn(|_| VecDeque::new())
}

/// The shared queue, and whether it still accepts jobs.
struct Injector {
    lanes: Lanes,
    /// Set once, when the pool has finished shutting down. Checked under the lock of every push.
    closed: bool,
}

/// All queues of one pool.
pub(crate) struct Queues {
    injector: Mutex<Injector>,
    /// One queue per worker, indexed by worker index. Empty for a pool without workers.
    locals: Vec<Mutex<Lanes>>,
    /// How many jobs sit in the queues, per lane. Raised *before* a job is pushed and lowered *after* it was
    /// popped, so it never goes below the true number and never wraps. Lets a search skip an empty lane without
    /// taking a single lock, and lets sleeping workers know whether there is anything to wake up for.
    queued: [AtomicUsize; LANES],
}

impl Queues {
    pub(crate) fn new(workers: usize) -> Self {
        Self {
            injector: Mutex::new(Injector {
                lanes: empty_lanes(),
                closed: false,
            }),
            locals: (0..workers).map(|_| Mutex::new(empty_lanes())).collect(),
            queued: std::array::from_fn(|_| AtomicUsize::new(0)),
        }
    }

    /// Queues a job that comes from a thread that is not a worker of this pool. Gives the job back when the
    /// pool has already closed.
    pub(crate) fn push_external(&self, priority: JobPriority, task: Task) -> Result<(), Task> {
        let lane = priority.lane();
        let mut injector = lock(&self.injector);
        if injector.closed {
            return Err(task);
        }
        self.queued[lane].fetch_add(1, Ordering::SeqCst);
        injector.lanes[lane].push_back(task);
        Ok(())
    }

    /// Queues a job spawned by worker `worker`, into that worker's own queue. Never refused: the worker is
    /// still running, and it will find the job (or another worker will steal it).
    pub(crate) fn push_local(&self, worker: usize, priority: JobPriority, task: Task) {
        let lane = priority.lane();
        self.queued[lane].fetch_add(1, Ordering::SeqCst);
        match self.locals.get(worker) {
            Some(local) => lock(local)[lane].push_back(task),
            // Not reachable (worker indices come from this pool), but a job must never be lost.
            None => lock(&self.injector).lanes[lane].push_back(task),
        }
    }

    /// Takes the next job for the thread `me` (`Some(worker index)` for a worker of this pool, `None` for any
    /// other thread that helps), following the rules in the module documentation.
    pub(crate) fn pop(&self, me: Option<usize>) -> Option<Task> {
        for lane in (0..LANES).rev() {
            if self.queued[lane].load(Ordering::SeqCst) == 0 {
                continue;
            }
            if let Some(task) = self.pop_lane(lane, me) {
                self.queued[lane].fetch_sub(1, Ordering::SeqCst);
                return Some(task);
            }
        }
        None
    }

    fn pop_lane(&self, lane: usize, me: Option<usize>) -> Option<Task> {
        if let Some(local) = me.and_then(|index| self.locals.get(index)) {
            let task = lock(local)[lane].pop_back();
            if task.is_some() {
                return task;
            }
        }

        let task = lock(&self.injector).lanes[lane].pop_front();
        if task.is_some() {
            return task;
        }

        // Steal. Start next to our own index so that the workers do not all hit worker 0 first.
        let count = self.locals.len();
        let start = me.map_or(0, |index| index + 1);
        for offset in 0..count {
            let victim = (start + offset) % count;
            if Some(victim) == me {
                continue;
            }
            let task = lock(&self.locals[victim])[lane].pop_front();
            if task.is_some() {
                return task;
            }
        }
        None
    }

    /// The number of queued jobs (running jobs are not counted).
    pub(crate) fn queued(&self) -> usize {
        self.queued
            .iter()
            .map(|count| count.load(Ordering::SeqCst))
            .sum()
    }

    /// Stops accepting outside jobs if nothing is queued anywhere. `true` once the queues are closed.
    pub(crate) fn close_if_empty(&self) -> bool {
        let mut injector = lock(&self.injector);
        if self.queued() == 0 {
            injector.closed = true;
        }
        injector.closed
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    /// A job that records `label` when it runs.
    fn labelled(log: &Arc<Mutex<Vec<&'static str>>>, label: &'static str) -> Task {
        let log = Arc::clone(log);
        Box::new(move || lock(&log).push(label))
    }

    fn drain(queues: &Queues, me: Option<usize>) -> usize {
        let mut ran = 0;
        while let Some(task) = queues.pop(me) {
            task();
            ran += 1;
        }
        ran
    }

    fn new_log() -> Arc<Mutex<Vec<&'static str>>> {
        Arc::new(Mutex::new(Vec::new()))
    }

    #[test]
    fn the_shared_queue_is_first_in_first_out_within_a_priority() {
        let queues = Queues::new(0);
        let log = new_log();
        for label in ["a", "b", "c"] {
            queues
                .push_external(JobPriority::Normal, labelled(&log, label))
                .ok()
                .unwrap();
        }
        assert_eq!(drain(&queues, None), 3);
        assert_eq!(*lock(&log), ["a", "b", "c"]);
    }

    #[test]
    fn higher_priorities_come_first_whatever_the_push_order() {
        let queues = Queues::new(1);
        let log = new_log();
        queues
            .push_external(JobPriority::Low, labelled(&log, "low"))
            .ok()
            .unwrap();
        queues.push_local(0, JobPriority::Normal, labelled(&log, "normal-local"));
        queues
            .push_external(JobPriority::High, labelled(&log, "high"))
            .ok()
            .unwrap();
        queues
            .push_external(JobPriority::Normal, labelled(&log, "normal-shared"))
            .ok()
            .unwrap();
        drain(&queues, Some(0));
        // Within Normal the worker's own queue comes before the shared one.
        assert_eq!(
            *lock(&log),
            ["high", "normal-local", "normal-shared", "low"]
        );
    }

    #[test]
    fn a_worker_takes_its_own_newest_job_first() {
        let queues = Queues::new(1);
        let log = new_log();
        for label in ["first", "second", "third"] {
            queues.push_local(0, JobPriority::Normal, labelled(&log, label));
        }
        drain(&queues, Some(0));
        assert_eq!(*lock(&log), ["third", "second", "first"]);
    }

    #[test]
    fn a_thief_takes_the_victims_oldest_job_first() {
        let queues = Queues::new(2);
        let log = new_log();
        for label in ["first", "second", "third"] {
            queues.push_local(0, JobPriority::Normal, labelled(&log, label));
        }
        // Worker 1 has nothing of its own and the shared queue is empty: it steals.
        drain(&queues, Some(1));
        assert_eq!(*lock(&log), ["first", "second", "third"]);
    }

    #[test]
    fn a_helper_that_is_not_a_worker_can_take_from_every_queue() {
        let queues = Queues::new(2);
        let log = new_log();
        queues.push_local(0, JobPriority::Normal, labelled(&log, "w0"));
        queues.push_local(1, JobPriority::Normal, labelled(&log, "w1"));
        queues
            .push_external(JobPriority::Normal, labelled(&log, "shared"))
            .ok()
            .unwrap();
        assert_eq!(drain(&queues, None), 3);
        assert_eq!(*lock(&log), ["shared", "w0", "w1"]);
    }

    #[test]
    fn a_worker_does_not_steal_from_itself_twice() {
        // `pop` for worker 0 must not walk into its own queue through the steal loop.
        let queues = Queues::new(1);
        assert!(queues.pop(Some(0)).is_none());
    }

    #[test]
    fn the_counters_follow_pushes_and_pops() {
        let queues = Queues::new(1);
        let log = new_log();
        assert_eq!(queues.queued(), 0);
        queues
            .push_external(JobPriority::Low, labelled(&log, "a"))
            .ok()
            .unwrap();
        queues.push_local(0, JobPriority::High, labelled(&log, "b"));
        assert_eq!(queues.queued(), 2);
        drain(&queues, Some(0));
        assert_eq!(queues.queued(), 0);
    }

    #[test]
    fn closing_needs_empty_queues_and_then_refuses_outside_jobs() {
        let queues = Queues::new(0);
        let log = new_log();
        queues
            .push_external(JobPriority::Normal, labelled(&log, "a"))
            .ok()
            .unwrap();
        assert!(!queues.close_if_empty(), "a job is still queued");
        drain(&queues, None);
        assert!(queues.close_if_empty());
        assert!(
            queues
                .push_external(JobPriority::Normal, labelled(&log, "late"))
                .is_err()
        );
        assert!(queues.close_if_empty(), "closing is permanent");
    }

    #[test]
    fn local_pushes_are_accepted_after_closing() {
        let queues = Queues::new(1);
        let log = new_log();
        assert!(queues.close_if_empty());
        queues.push_local(0, JobPriority::Normal, labelled(&log, "child"));
        assert_eq!(drain(&queues, Some(0)), 1);
    }
}
