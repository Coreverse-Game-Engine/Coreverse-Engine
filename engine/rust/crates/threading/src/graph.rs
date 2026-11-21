//! Task graphs: tasks with dependencies, checked for cycles while the graph is built and run in parallel with
//! counters.
//!
//! Everything here is safe code. A graph run is a [scope](crate::Scope): the run does not return before every
//! task has run (or has been skipped), so tasks may borrow from the caller's stack exactly like scoped jobs.
//!
//! # How a run works
//!
//! Every task has a counter of the dependencies that have not finished yet. A run starts the tasks whose
//! counter is 0 (the roots). When a task finishes it lowers the counter of each of its dependents; the one
//! that brings a counter to 0 queues that dependent as a scoped job. No thread ever waits for a dependency
//! (a worker is never blocked by a task that is not ready), and the thread that called the run helps like any
//! thread that waits for a scope.
//!
//! A task that panics does not stop the run: independent tasks still run. The tasks that depend on it
//! (directly or not) are *skipped*, because their input is not there; they never run and are counted in
//! [`GraphPanic::skipped_tasks`]. After everything else has finished, the run re-raises the panic of the
//! failed task with the smallest [`TaskId::index`] (the same one on every run), or returns it as a
//! [`GraphPanic`].

use std::borrow::Cow;
use std::collections::VecDeque;
use std::fmt;
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use crate::error::{ThreadingError, ThreadingResult};
use crate::job::{JobPanic, PanicPayload, discard_payload};
use crate::pool::{Shared, Spawner, ThreadPool};
use crate::priority::JobPriority;
use crate::scope::{Scope, run_scope};
use crate::sync::lock;

/// Hands out a different id to every graph, so a [`TaskId`] can tell which graph made it.
static NEXT_GRAPH_ID: AtomicUsize = AtomicUsize::new(0);

/// Names one task of a [`TaskGraph`]. Handed out by [`TaskGraph::add`] and [`TaskGraph::add_after`]; it is
/// only good for the graph that made it (other graphs answer [`ThreadingError::UnknownTask`]).
#[derive(Copy, Clone, Eq, PartialEq, Hash)]
pub struct TaskId {
    graph: usize,
    index: usize,
}

impl TaskId {
    /// The position of the task in the order the tasks were added to its graph (0 for the first). It is also
    /// the order that decides which panic a run reports when several tasks panic.
    #[must_use]
    pub fn index(self) -> usize {
        self.index
    }
}

impl fmt::Debug for TaskId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TaskId({})", self.index)
    }
}

/// What the graph knows about a task besides its closure.
struct Node {
    name: Cow<'static, str>,
    priority: JobPriority,
    /// The tasks that must finish before this one starts, each at most once.
    dependencies: Vec<usize>,
    /// The tasks that wait for this one, in the order the edges were added, each at most once.
    dependents: Vec<usize>,
}

/// A directed acyclic graph of tasks: closures, and which of them must finish before which others start.
///
/// Build the graph with [`add`](Self::add), [`add_after`](Self::add_after) and [`depend`](Self::depend), then
/// run it with [`ThreadPool::run_graph`] (or [`Spawner::run_graph`]). A graph that is built cannot contain a
/// cycle: [`depend`](Self::depend) refuses an edge that would close one and returns
/// [`ThreadingError::DependencyCycle`] with the path, leaving the graph unchanged.
///
/// ```
/// use std::sync::Mutex;
/// use threading::{PoolConfig, TaskGraph, ThreadPool};
///
/// let pool = ThreadPool::new(PoolConfig::new())?;
/// let log = Mutex::new(Vec::new());
///
/// let mut graph = TaskGraph::new();
/// let load = graph.add("load", || log.lock().unwrap().push("load"));
/// let physics = graph.add_after("physics", &[load], || log.lock().unwrap().push("physics"))?;
/// let animation = graph.add_after("animation", &[load], || log.lock().unwrap().push("animation"))?;
/// graph.add_after("render", &[physics, animation], || log.lock().unwrap().push("render"))?;
///
/// // The tasks borrow `log` from this stack frame; the run returns after all of them finished.
/// pool.run_graph(&mut graph);
/// drop(graph);
///
/// let log = log.into_inner().unwrap();
/// assert_eq!(log.first(), Some(&"load"));
/// assert_eq!(log.last(), Some(&"render"));
/// assert_eq!(log.len(), 4);
/// # Ok::<(), threading::ThreadingError>(())
/// ```
///
/// A cycle is an error, and the graph stays usable:
///
/// ```
/// use threading::TaskGraph;
///
/// let mut graph = TaskGraph::new();
/// let a = graph.add("a", || ());
/// let b = graph.add_after("b", &[a], || ())?;
/// let err = graph.depend(a, b).unwrap_err(); // `a` would have to run after `b`
/// assert_eq!(err.to_string(), "the dependency would create a cycle: a -> b -> a");
/// assert_eq!(graph.len(), 2);
/// # Ok::<(), threading::ThreadingError>(())
/// ```
///
/// # Tasks and reuse
///
/// A task is an `FnMut() + Send` closure that returns nothing: it hands results back through the data it
/// borrows (a `&mut` to a slot, an atomic, a `Mutex`), the way [`Scope::spawn`] jobs do. `FnMut` (not
/// `FnOnce`) lets a graph be run again and again, every frame say, without being built again; each run calls
/// every task at most once. A closure that has to move a value out of what it captured wraps it in an
/// `Option` and `take`s it.
///
/// `'env` is the lifetime of what the tasks borrow; the graph cannot outlive it.
pub struct TaskGraph<'env> {
    id: usize,
    nodes: Vec<Node>,
    jobs: Vec<Box<dyn FnMut() + Send + 'env>>,
}

impl Default for TaskGraph<'_> {
    fn default() -> Self {
        Self::new()
    }
}

impl<'env> TaskGraph<'env> {
    /// An empty graph.
    #[must_use]
    pub fn new() -> Self {
        Self::with_capacity(0)
    }

    /// An empty graph with room for `tasks` tasks.
    #[must_use]
    pub fn with_capacity(tasks: usize) -> Self {
        Self {
            id: NEXT_GRAPH_ID.fetch_add(1, Ordering::Relaxed),
            nodes: Vec::with_capacity(tasks),
            jobs: Vec::with_capacity(tasks),
        }
    }

    /// The number of tasks.
    #[must_use]
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Whether the graph has no task.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Adds a task without dependencies (it can start as soon as a run begins) with
    /// [`JobPriority::Normal`]. The name is for errors, panic reports and tools; it need not be unique.
    pub fn add<N, F>(&mut self, name: N, job: F) -> TaskId
    where
        N: Into<Cow<'static, str>>,
        F: FnMut() + Send + 'env,
    {
        self.push(name.into(), Vec::new(), job)
    }

    /// Adds a task that starts only after every task of `dependencies` has finished successfully.
    ///
    /// A new task has no dependents yet, so it cannot close a cycle; the only error is an id that does not
    /// belong to this graph.
    ///
    /// # Errors
    ///
    /// [`ThreadingError::UnknownTask`] if one of the ids was made by another graph. Nothing is added then.
    pub fn add_after<N, F>(
        &mut self,
        name: N,
        dependencies: &[TaskId],
        job: F,
    ) -> ThreadingResult<TaskId>
    where
        N: Into<Cow<'static, str>>,
        F: FnMut() + Send + 'env,
    {
        let mut indices = dependencies
            .iter()
            .map(|&id| self.index_of(id))
            .collect::<ThreadingResult<Vec<usize>>>()?;
        indices.sort_unstable();
        indices.dedup();
        Ok(self.push(name.into(), indices, job))
    }

    fn push<F>(&mut self, name: Cow<'static, str>, dependencies: Vec<usize>, job: F) -> TaskId
    where
        F: FnMut() + Send + 'env,
    {
        let index = self.nodes.len();
        for &dependency in &dependencies {
            self.nodes[dependency].dependents.push(index);
        }
        self.nodes.push(Node {
            name,
            priority: JobPriority::Normal,
            dependencies,
            dependents: Vec::new(),
        });
        self.jobs.push(Box::new(job));
        self.id_of(index)
    }

    /// Makes `task` start only after `after` has finished successfully. Adding an edge that is there already
    /// does nothing.
    ///
    /// # Errors
    ///
    /// * [`ThreadingError::DependencyCycle`] if `after` already (directly or not) waits for `task`, or if
    ///   they are the same task. The error names the tasks on the cycle; the graph is left unchanged.
    /// * [`ThreadingError::UnknownTask`] if one of the ids was made by another graph.
    pub fn depend(&mut self, task: TaskId, after: TaskId) -> ThreadingResult<()> {
        let task = self.index_of(task)?;
        let after = self.index_of(after)?;
        if self.nodes[task].dependencies.contains(&after) {
            return Ok(());
        }
        // The new edge `after -> task` closes a cycle exactly when `task` already leads to `after`.
        if let Some(path) = self.path(task, after) {
            let mut names: Vec<String> = path
                .iter()
                .map(|&index| self.nodes[index].name.to_string())
                .collect();
            names.push(self.nodes[task].name.to_string());
            return Err(ThreadingError::DependencyCycle { path: names });
        }
        self.nodes[task].dependencies.push(after);
        self.nodes[after].dependents.push(task);
        Ok(())
    }

    /// Sets the priority the task is queued with (default [`JobPriority::Normal`]). It decides the order
    /// among tasks that are ready at the same time; it never lets a task start before its dependencies.
    ///
    /// # Errors
    ///
    /// [`ThreadingError::UnknownTask`] if the id was made by another graph.
    pub fn set_priority(&mut self, task: TaskId, priority: JobPriority) -> ThreadingResult<()> {
        let index = self.index_of(task)?;
        self.nodes[index].priority = priority;
        Ok(())
    }

    /// The name of a task.
    ///
    /// # Errors
    ///
    /// [`ThreadingError::UnknownTask`] if the id was made by another graph.
    pub fn name(&self, task: TaskId) -> ThreadingResult<&str> {
        Ok(&self.nodes[self.index_of(task)?].name)
    }

    /// The priority of a task.
    ///
    /// # Errors
    ///
    /// [`ThreadingError::UnknownTask`] if the id was made by another graph.
    pub fn priority(&self, task: TaskId) -> ThreadingResult<JobPriority> {
        Ok(self.nodes[self.index_of(task)?].priority)
    }

    /// The tasks that must finish before `task` starts, in the order of their ids.
    ///
    /// # Errors
    ///
    /// [`ThreadingError::UnknownTask`] if the id was made by another graph.
    pub fn dependencies(&self, task: TaskId) -> ThreadingResult<Vec<TaskId>> {
        let index = self.index_of(task)?;
        let mut ids: Vec<TaskId> = self.nodes[index]
            .dependencies
            .iter()
            .map(|&dependency| self.id_of(dependency))
            .collect();
        ids.sort_unstable_by_key(|id| id.index);
        Ok(ids)
    }

    /// The tasks that wait for `task`, in the order of their ids.
    ///
    /// # Errors
    ///
    /// [`ThreadingError::UnknownTask`] if the id was made by another graph.
    pub fn dependents(&self, task: TaskId) -> ThreadingResult<Vec<TaskId>> {
        let index = self.index_of(task)?;
        let mut ids: Vec<TaskId> = self.nodes[index]
            .dependents
            .iter()
            .map(|&dependent| self.id_of(dependent))
            .collect();
        ids.sort_unstable_by_key(|id| id.index);
        Ok(ids)
    }

    /// Every task once, so that each task comes after all of its dependencies. Among the tasks that are free
    /// to go next, the earlier ones in the order they became free come first; the order is the same on every
    /// call. A pool without workers runs a graph in this order (when all priorities are equal).
    #[must_use]
    pub fn topological_order(&self) -> Vec<TaskId> {
        let mut waiting: Vec<usize> = self
            .nodes
            .iter()
            .map(|node| node.dependencies.len())
            .collect();
        let mut ready: VecDeque<usize> = waiting
            .iter()
            .enumerate()
            .filter_map(|(index, &count)| (count == 0).then_some(index))
            .collect();
        let mut order = Vec::with_capacity(self.nodes.len());
        while let Some(index) = ready.pop_front() {
            order.push(self.id_of(index));
            for &dependent in &self.nodes[index].dependents {
                waiting[dependent] -= 1;
                if waiting[dependent] == 0 {
                    ready.push_back(dependent);
                }
            }
        }
        order
    }

    fn id_of(&self, index: usize) -> TaskId {
        TaskId {
            graph: self.id,
            index,
        }
    }

    fn index_of(&self, id: TaskId) -> ThreadingResult<usize> {
        if id.graph == self.id && id.index < self.nodes.len() {
            Ok(id.index)
        } else {
            Err(ThreadingError::UnknownTask)
        }
    }

    /// A path of dependents from `from` to `to` (both included; just `[from]` when they are the same task),
    /// if there is one.
    fn path(&self, from: usize, to: usize) -> Option<Vec<usize>> {
        if from == to {
            return Some(vec![from]);
        }
        let mut parent = vec![usize::MAX; self.nodes.len()];
        let mut seen = vec![false; self.nodes.len()];
        seen[from] = true;
        let mut stack = vec![from];
        while let Some(current) = stack.pop() {
            for &next in &self.nodes[current].dependents {
                if seen[next] {
                    continue;
                }
                seen[next] = true;
                parent[next] = current;
                if next == to {
                    let mut path = vec![to];
                    let mut at = to;
                    while at != from {
                        at = parent[at];
                        path.push(at);
                    }
                    path.reverse();
                    return Some(path);
                }
                stack.push(next);
            }
        }
        None
    }
}

impl fmt::Debug for TaskGraph<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let tasks: Vec<&str> = self.nodes.iter().map(|node| &*node.name).collect();
        f.debug_struct("TaskGraph")
            .field("tasks", &tasks)
            .finish_non_exhaustive()
    }
}

/// A task of a graph run panicked.
///
/// Returned by [`ThreadPool::run_graph_catch`] and [`Spawner::run_graph_catch`]. If several tasks panicked,
/// this is the one with the smallest [`TaskId::index`]; the other panic payloads are dropped.
pub struct GraphPanic {
    task: TaskId,
    name: String,
    panic: JobPanic,
    skipped: usize,
}

impl GraphPanic {
    /// The task that panicked.
    #[must_use]
    pub fn task(&self) -> TaskId {
        self.task
    }

    /// The name of the task that panicked.
    #[must_use]
    pub fn task_name(&self) -> &str {
        &self.name
    }

    /// The panic of the task.
    #[must_use]
    pub fn panic(&self) -> &JobPanic {
        &self.panic
    }

    /// How many tasks did not run because they depend, directly or not, on a task that panicked (counting
    /// those of every panicked task, not only this one).
    #[must_use]
    pub fn skipped_tasks(&self) -> usize {
        self.skipped
    }

    /// Gives back the panic of the task, for [`JobPanic::into_payload`] and re-raising.
    #[must_use]
    pub fn into_panic(self) -> JobPanic {
        self.panic
    }
}

impl fmt::Debug for GraphPanic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GraphPanic")
            .field("task", &self.task)
            .field("name", &self.name)
            .field("message", &self.panic.message())
            .field("skipped", &self.skipped)
            .finish()
    }
}

impl fmt::Display for GraphPanic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.panic.message() {
            Some(message) => write!(f, "task `{}` panicked: {message}", self.name)?,
            None => write!(f, "task `{}` panicked with a non-string payload", self.name)?,
        }
        if self.skipped > 0 {
            write!(f, " ({} dependent tasks skipped)", self.skipped)?;
        }
        Ok(())
    }
}

impl std::error::Error for GraphPanic {}

/// What one run of a graph shares between its tasks.
struct Run<'a, 'g> {
    nodes: &'a [Node],
    /// The closure of every task, until the task starts. Each is taken exactly once (under a lock that is
    /// not held while the closure runs), so no two threads ever hold the same `&mut`.
    jobs: Vec<Mutex<Option<&'a mut (dyn FnMut() + Send + 'g)>>>,
    /// How many dependencies of every task have not finished yet.
    remaining: Vec<AtomicUsize>,
    /// Set on a task when one of its dependencies failed or was skipped.
    poisoned: Vec<AtomicBool>,
    skipped: AtomicUsize,
    /// The panics of the tasks that ran, with their indices.
    panics: Mutex<Vec<(usize, PanicPayload)>>,
}

/// Queues the task `index`, which is ready, as a scoped job.
fn spawn_task<'scope, 'env>(
    scope: &'scope Scope<'scope, 'env>,
    run: &'scope Run<'_, '_>,
    index: usize,
) {
    scope.spawn_with_priority(run.nodes[index].priority, move || {
        execute(scope, run, index);
    });
}

/// Runs the task `index` and releases the tasks that wait for it.
fn execute<'scope, 'env>(
    scope: &'scope Scope<'scope, 'env>,
    run: &'scope Run<'_, '_>,
    index: usize,
) {
    let job = lock(&run.jobs[index]).take();
    let mut failed = false;
    if let Some(job) = job
        && let Err(payload) = panic::catch_unwind(AssertUnwindSafe(job))
    {
        failed = true;
        lock(&run.panics).push((index, payload));
    }
    complete_task(scope, run, index, failed);
}

/// Lowers the counters of the dependents of a finished task, and does the same for the tasks that turn out to
/// be skipped because of it.
fn complete_task<'scope, 'env>(
    scope: &'scope Scope<'scope, 'env>,
    run: &'scope Run<'_, '_>,
    index: usize,
    failed: bool,
) {
    let mut skipped = Vec::new();
    release_dependents(scope, run, index, failed, &mut skipped);
    while let Some(index) = skipped.pop() {
        run.skipped.fetch_add(1, Ordering::Relaxed);
        release_dependents(scope, run, index, true, &mut skipped);
    }
}

fn release_dependents<'scope, 'env>(
    scope: &'scope Scope<'scope, 'env>,
    run: &'scope Run<'_, '_>,
    index: usize,
    failed: bool,
    skipped: &mut Vec<usize>,
) {
    for &dependent in &run.nodes[index].dependents {
        if failed {
            run.poisoned[dependent].store(true, Ordering::Release);
        }
        // The thread that brings the counter to 0 sees every earlier decrement (and the poison flag stored
        // before it) and, through the queue, hands the dependent to whoever runs it.
        if run.remaining[dependent].fetch_sub(1, Ordering::AcqRel) == 1 {
            if run.poisoned[dependent].load(Ordering::Acquire) {
                skipped.push(dependent);
            } else {
                spawn_task(scope, run, dependent);
            }
        }
    }
}

/// Runs every task of `graph` on the pool and the calling thread.
fn run_graph(shared: &Arc<Shared>, graph: &mut TaskGraph<'_>) -> Result<(), GraphPanic> {
    if graph.nodes.is_empty() {
        return Ok(());
    }
    let graph_id = graph.id;
    let TaskGraph { nodes, jobs, .. } = graph;
    let run = Run {
        nodes: &nodes[..],
        jobs: jobs
            .iter_mut()
            .map(|job| Mutex::new(Some(&mut **job)))
            .collect(),
        remaining: nodes
            .iter()
            .map(|node| AtomicUsize::new(node.dependencies.len()))
            .collect(),
        poisoned: nodes.iter().map(|_| AtomicBool::new(false)).collect(),
        skipped: AtomicUsize::new(0),
        panics: Mutex::new(Vec::new()),
    };

    run_scope(shared, |scope| {
        for (index, node) in run.nodes.iter().enumerate() {
            if node.dependencies.is_empty() {
                spawn_task(scope, &run, index);
            }
        }
    });

    let skipped = run.skipped.load(Ordering::Relaxed);
    let mut panics = run
        .panics
        .into_inner()
        .unwrap_or_else(PoisonError::into_inner);
    if panics.is_empty() {
        return Ok(());
    }
    panics.sort_by_key(|&(index, _)| index);
    let mut panics = panics.into_iter();
    let (index, payload) = panics.next().expect("the list is not empty");
    for (_, other) in panics {
        discard_payload(other);
    }
    Err(GraphPanic {
        task: TaskId {
            graph: graph_id,
            index,
        },
        name: nodes[index].name.to_string(),
        panic: JobPanic::new(payload),
        skipped,
    })
}

fn run_graph_or_raise(shared: &Arc<Shared>, graph: &mut TaskGraph<'_>) {
    if let Err(failure) = run_graph(shared, graph) {
        panic::resume_unwind(failure.into_panic().into_payload());
    }
}

impl ThreadPool {
    /// Runs every task of `graph`, each after all of its dependencies, spread over the workers and the
    /// calling thread, and returns when all of them are done.
    ///
    /// The calling thread helps while it waits, so this works on a pool without workers too, and it may be
    /// called from inside a job. Tasks may borrow from the caller's stack. The graph can be run again
    /// afterwards.
    ///
    /// # Panics
    ///
    /// If a task panics, the tasks that do not depend on it still run, the tasks that do are skipped, and
    /// then the panic of the failed task with the smallest [`TaskId::index`] is re-raised with its original
    /// payload. [`run_graph_catch`](Self::run_graph_catch) returns it as a value instead.
    pub fn run_graph(&self, graph: &mut TaskGraph<'_>) {
        run_graph_or_raise(&self.shared, graph);
    }

    /// Like [`run_graph`](Self::run_graph), but a task panic is returned as a [`GraphPanic`] instead of being
    /// re-raised.
    ///
    /// ```
    /// use threading::{PoolConfig, TaskGraph, ThreadPool};
    ///
    /// let pool = ThreadPool::new(PoolConfig::new())?;
    /// let mut graph = TaskGraph::new();
    /// let bad = graph.add("bad", || panic!("no input"));
    /// graph.add_after("after", &[bad], || unreachable!("skipped"))?;
    ///
    /// let failure = pool.run_graph_catch(&mut graph).unwrap_err();
    /// assert_eq!(failure.task_name(), "bad");
    /// assert_eq!(failure.panic().message(), Some("no input"));
    /// assert_eq!(failure.skipped_tasks(), 1);
    /// # Ok::<(), threading::ThreadingError>(())
    /// ```
    ///
    /// # Errors
    ///
    /// [`GraphPanic`] when a task panicked (see [`run_graph`](Self::run_graph) for which one is reported).
    pub fn run_graph_catch(&self, graph: &mut TaskGraph<'_>) -> Result<(), GraphPanic> {
        run_graph(&self.shared, graph)
    }
}

impl Spawner {
    /// Like [`ThreadPool::run_graph`]: runs every task of `graph` and returns when all are done. A job that
    /// carries a spawner can run a graph this way.
    ///
    /// # Panics
    ///
    /// Re-raises the panic of a failed task, as [`ThreadPool::run_graph`] does.
    pub fn run_graph(&self, graph: &mut TaskGraph<'_>) {
        run_graph_or_raise(&self.shared, graph);
    }

    /// Like [`ThreadPool::run_graph_catch`].
    ///
    /// # Errors
    ///
    /// [`GraphPanic`] when a task panicked.
    pub fn run_graph_catch(&self, graph: &mut TaskGraph<'_>) -> Result<(), GraphPanic> {
        run_graph(&self.shared, graph)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(graph: &TaskGraph<'_>, ids: &[TaskId]) -> Vec<String> {
        ids.iter()
            .map(|&id| graph.name(id).unwrap().to_owned())
            .collect()
    }

    fn diamond<'a>() -> (TaskGraph<'a>, [TaskId; 4]) {
        let mut graph = TaskGraph::new();
        let a = graph.add("a", || ());
        let b = graph.add_after("b", &[a], || ()).unwrap();
        let c = graph.add_after("c", &[a], || ()).unwrap();
        let d = graph.add_after("d", &[b, c], || ()).unwrap();
        (graph, [a, b, c, d])
    }

    #[test]
    fn tasks_get_consecutive_indices_and_names() {
        let (graph, ids) = diamond();
        assert_eq!(graph.len(), 4);
        assert!(!graph.is_empty());
        let indices: Vec<usize> = ids.iter().map(|id| id.index()).collect();
        assert_eq!(indices, [0, 1, 2, 3]);
        assert_eq!(names(&graph, &ids), ["a", "b", "c", "d"]);
        assert_eq!(format!("{:?}", ids[2]), "TaskId(2)");
    }

    #[test]
    fn dependencies_and_dependents_mirror_each_other() {
        let (graph, [a, b, c, d]) = diamond();
        assert_eq!(graph.dependencies(a).unwrap(), []);
        assert_eq!(graph.dependents(a).unwrap(), [b, c]);
        assert_eq!(graph.dependencies(d).unwrap(), [b, c]);
        assert_eq!(graph.dependents(d).unwrap(), []);
    }

    #[test]
    fn duplicate_dependencies_are_stored_once() {
        let mut graph = TaskGraph::new();
        let a = graph.add("a", || ());
        let b = graph.add_after("b", &[a, a], || ()).unwrap();
        graph.depend(b, a).unwrap();
        graph.depend(b, a).unwrap();
        assert_eq!(graph.dependencies(b).unwrap(), [a]);
        assert_eq!(graph.dependents(a).unwrap(), [b]);
    }

    #[test]
    fn the_topological_order_puts_every_task_after_its_dependencies() {
        let (graph, ids) = diamond();
        let order = graph.topological_order();
        assert_eq!(order.len(), 4);
        assert_eq!(names(&graph, &order), ["a", "b", "c", "d"]);
        for &id in &ids {
            let position = order.iter().position(|&other| other == id).unwrap();
            for dependency in graph.dependencies(id).unwrap() {
                let earlier = order.iter().position(|&other| other == dependency).unwrap();
                assert!(earlier < position);
            }
        }
    }

    #[test]
    fn a_task_cannot_depend_on_itself() {
        let mut graph = TaskGraph::new();
        let a = graph.add("a", || ());
        let err = graph.depend(a, a).unwrap_err();
        assert!(
            matches!(&err, ThreadingError::DependencyCycle { path } if path == &["a", "a"]),
            "{err}"
        );
        assert_eq!(graph.dependencies(a).unwrap(), []);
    }

    #[test]
    fn a_cycle_names_the_whole_path_and_leaves_the_graph_unchanged() {
        let mut graph = TaskGraph::new();
        let a = graph.add("a", || ());
        let b = graph.add_after("b", &[a], || ()).unwrap();
        let c = graph.add_after("c", &[b], || ()).unwrap();
        let err = graph.depend(a, c).unwrap_err();
        match err {
            ThreadingError::DependencyCycle { path } => assert_eq!(path, ["a", "b", "c", "a"]),
            other => panic!("unexpected error: {other}"),
        }
        assert_eq!(graph.dependencies(a).unwrap(), []);
        assert_eq!(graph.dependents(c).unwrap(), []);
        // The refused edge left the graph usable, and a harmless one is still accepted.
        let d = graph.add_after("d", &[c], || ()).unwrap();
        graph.depend(d, a).unwrap();
        assert_eq!(graph.topological_order().len(), 4);
    }

    #[test]
    fn the_cycle_path_follows_a_branch_not_the_whole_graph() {
        let mut graph = TaskGraph::new();
        let a = graph.add("a", || ());
        let side = graph.add_after("side", &[a], || ()).unwrap();
        let b = graph.add_after("b", &[a], || ()).unwrap();
        let c = graph.add_after("c", &[b], || ()).unwrap();
        let _ = side;
        let err = graph.depend(a, c).unwrap_err();
        match err {
            ThreadingError::DependencyCycle { path } => assert_eq!(path, ["a", "b", "c", "a"]),
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn an_id_from_another_graph_is_unknown() {
        let mut first = TaskGraph::new();
        let foreign = first.add("foreign", || ());
        let mut second = TaskGraph::new();
        let local = second.add("local", || ());
        assert!(matches!(
            second.add_after("x", &[foreign], || ()),
            Err(ThreadingError::UnknownTask)
        ));
        assert_eq!(second.len(), 1, "a refused add must not add anything");
        assert!(matches!(
            second.depend(local, foreign),
            Err(ThreadingError::UnknownTask)
        ));
        assert!(matches!(
            second.name(foreign),
            Err(ThreadingError::UnknownTask)
        ));
        assert!(matches!(
            second.set_priority(foreign, JobPriority::High),
            Err(ThreadingError::UnknownTask)
        ));
    }

    #[test]
    fn priorities_default_to_normal_and_can_be_set() {
        let mut graph = TaskGraph::new();
        let a = graph.add("a", || ());
        assert_eq!(graph.priority(a).unwrap(), JobPriority::Normal);
        graph.set_priority(a, JobPriority::High).unwrap();
        assert_eq!(graph.priority(a).unwrap(), JobPriority::High);
    }

    #[test]
    fn debug_lists_the_task_names() {
        let (graph, _) = diamond();
        let text = format!("{graph:?}");
        assert!(text.contains("\"a\"") && text.contains("\"d\""), "{text}");
    }

    #[test]
    fn graph_panic_prints_task_message_and_skips() {
        let failure = GraphPanic {
            task: TaskId { graph: 0, index: 1 },
            name: "physics".to_owned(),
            panic: JobPanic::new(Box::new("diverged")),
            skipped: 2,
        };
        assert_eq!(
            failure.to_string(),
            "task `physics` panicked: diverged (2 dependent tasks skipped)"
        );
        assert_eq!(failure.task().index(), 1);
        assert!(format!("{failure:?}").contains("physics"));
    }
}
