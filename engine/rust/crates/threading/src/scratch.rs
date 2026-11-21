//! Scratch memory: a [`FrameArena`] of its own for every worker, for the temporary data of a job. The user-facing
//! description (and the rules that make it safe) is on [`with_scratch`].

use std::cell::RefCell;
use std::rc::Rc;

use memory::FrameArena;

thread_local! {
    /// The arena of the calling thread, if it is a worker of a pool with scratch memory. An `Rc` so that
    /// [`with_scratch`] can take a clone and release the `RefCell` before it runs user code, and so that the
    /// worker can tell, with `Rc::get_mut`, whether anybody still holds the arena.
    static SCRATCH: RefCell<Option<Rc<FrameArena>>> = const { RefCell::new(None) };
}

/// Gives the calling thread its arena (a worker does this once, when it starts).
pub(crate) fn install(arena: FrameArena) {
    SCRATCH.with(|slot| *slot.borrow_mut() = Some(Rc::new(arena)));
}

/// Takes the arena away and drops it (a worker does this when it leaves), which gives the memory back.
pub(crate) fn uninstall() {
    // `try_with`: tolerate a thread that is already being torn down.
    let _ = SCRATCH.try_with(|slot| slot.borrow_mut().take());
}

/// Starts a new frame in the calling thread's arena: remembers how much the frame used, then frees it all.
///
/// Returns the bytes the frame that just ended used, or `None` when the thread has no arena or a
/// [`with_scratch`] closure holds it right now (then nothing is reset).
pub(crate) fn begin_frame() -> Option<usize> {
    SCRATCH.with(|slot| {
        let mut slot = slot.borrow_mut();
        let arena = Rc::get_mut(slot.as_mut()?)?;
        arena.begin_frame();
        Some(arena.last_frame_used())
    })
}

/// Runs `f` with the scratch arena of the calling thread and returns what `f` returns, or `None` when the thread
/// has no scratch arena.
///
/// A pool built with [`PoolConfig::with_scratch`](crate::PoolConfig::with_scratch) gives each worker thread a
/// [`FrameArena`] of its own, allocated when the pool starts and registered with a
/// [`MemoryTag`](memory::MemoryTag) so that it shows up in memory reports. A job reaches the arena of the thread
/// it runs on with this function:
///
/// ```
/// use std::sync::mpsc;
/// use threading::{PoolConfig, ThreadPool, WorkerCount, with_scratch};
///
/// let pool = ThreadPool::new(
///     PoolConfig::new()
///         .with_workers(WorkerCount::Exact(1))
///         .with_scratch(64 * 1024),
/// )?;
///
/// let (sender, receiver) = mpsc::channel();
/// drop(pool.spawn(move || {
///     // Bump-allocated, freed all at once at the next frame, no `malloc` per job.
///     let total = with_scratch(|arena| {
///         let numbers = arena.alloc_slice_fill(100, 2_u32).expect("room for 100 numbers");
///         numbers.iter().sum::<u32>()
///     });
///     sender.send(total).unwrap();
/// }));
/// assert_eq!(receiver.recv().unwrap(), Some(200));
/// # Ok::<(), threading::ThreadingError>(())
/// ```
///
/// # The rules that make it safe
///
/// * **Nothing outlives the closure.** The arena is lent for the duration of the closure only
///   (`FnOnce(&FrameArena) -> R`, with `R` independent of the borrow), so a reference into scratch memory
///   cannot leave it. What the job wants to keep it copies out into the closure's result.
/// * **Resets happen between jobs, never under a borrow.**
///   [`ThreadPool::begin_scratch_frame`](crate::ThreadPool::begin_scratch_frame) does not touch any arena; it
///   publishes "a new frame has begun". Each worker resets its own arena the next time it is between jobs, and
///   only when no closure holds the arena. A job that is running while the frame changes keeps its memory
///   until it is done, and the worker resets afterwards. All of this is safe code: the arena lives in a
///   thread-local `Rc`, and the exclusive borrow that [`FrameArena::begin_frame`] needs is obtained only when
///   that `Rc` has no other owner.
/// * **Nested use is fine.** A job that waits and runs other jobs meanwhile may call `with_scratch` in each of
///   them, and inside another `with_scratch`: the arena hands out disjoint pieces, and none of it is reset
///   until the outermost borrow ends.
/// * **Only workers have an arena.** The answer is `None` on any other thread: a thread that waits for a job
///   and helps may run the job itself, and a pool without workers has no worker threads at all. Code that can
///   run there needs a fallback, for example the heap.
///
/// If `f` panics, the panic goes on unchanged and the arena stays usable.
pub fn with_scratch<R>(f: impl FnOnce(&FrameArena) -> R) -> Option<R> {
    // Clone the `Rc` out so that no `RefCell` borrow is held while `f` runs (`f` may call `with_scratch` again).
    let arena = SCRATCH.with(|slot| slot.borrow().as_ref().map(Rc::clone))?;
    Some(f(&arena))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arena(bytes: usize) -> FrameArena {
        FrameArena::with_capacity(bytes).expect("a small arena")
    }

    fn on_a_fresh_thread<R: Send + 'static>(job: impl FnOnce() -> R + Send + 'static) -> R {
        std::thread::spawn(job).join().unwrap()
    }

    #[test]
    fn a_thread_without_an_arena_gets_none() {
        on_a_fresh_thread(|| {
            assert!(with_scratch(|_| ()).is_none());
            assert!(begin_frame().is_none());
        });
    }

    #[test]
    fn an_installed_arena_serves_with_scratch_until_uninstalled() {
        on_a_fresh_thread(|| {
            install(arena(4096));
            let value = with_scratch(|arena| *arena.alloc(41_u32).unwrap() + 1);
            assert_eq!(value, Some(42));
            uninstall();
            assert!(with_scratch(|_| ()).is_none());
        });
    }

    #[test]
    fn a_new_frame_frees_the_memory_and_reports_the_usage() {
        on_a_fresh_thread(|| {
            install(arena(4096));
            with_scratch(|arena| {
                arena.alloc_slice_fill(100, 0_u8).unwrap();
            });
            assert_eq!(begin_frame(), Some(100));
            let used = with_scratch(|arena| arena.used());
            assert_eq!(used, Some(0), "the frame started empty");
            uninstall();
        });
    }

    #[test]
    fn nothing_is_reset_while_a_closure_holds_the_arena() {
        on_a_fresh_thread(|| {
            install(arena(4096));
            with_scratch(|arena| {
                let slot = arena.alloc(7_u64).unwrap();
                assert_eq!(begin_frame(), None, "the arena is in use");
                // Nested use is fine, and the first allocation is still intact afterwards.
                with_scratch(|inner| *inner.alloc(1_u64).unwrap() += 1);
                assert_eq!(*slot, 7);
            });
            assert!(
                begin_frame().is_some(),
                "free again once the closure is done"
            );
            uninstall();
        });
    }

    #[test]
    fn a_panic_in_the_closure_leaves_the_arena_usable() {
        on_a_fresh_thread(|| {
            install(arena(4096));
            let caught = std::panic::catch_unwind(|| with_scratch(|_| panic!("boom")));
            assert!(caught.is_err());
            assert!(begin_frame().is_some(), "the borrow was released");
            assert_eq!(with_scratch(|arena| arena.used()), Some(0));
            uninstall();
        });
    }
}
