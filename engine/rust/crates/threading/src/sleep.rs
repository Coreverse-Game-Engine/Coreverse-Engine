//! Putting idle workers to sleep and waking them without losing a wake-up.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, PoisonError};

use crate::sync::lock;

/// The place where idle workers wait for work.
///
/// A worker that found nothing to do calls [`wait_unless`](Self::wait_unless); whoever makes work available
/// (after counting it in the queue, see `Queues`) calls [`wake_one`](Self::wake_one). The two sides cannot miss
/// each other:
///
/// * The sleeper takes the lock, announces itself in `sleepers`, and only then checks, under the lock, whether
///   it still has to sleep.
/// * The waker counts the work first, then reads `sleepers`; if somebody sleeps it takes the same lock before
///   notifying, which it can only do once the sleeper is really waiting (the wait releases the lock).
///
/// With sequentially consistent operations on the work counter and on `sleepers`, either the sleeper's check
/// sees the new work, or the waker sees the sleeper. The fast path (nobody sleeps) is a single atomic load.
pub(crate) struct Sleep {
    lock: Mutex<()>,
    wake: Condvar,
    sleepers: AtomicUsize,
}

impl Sleep {
    pub(crate) fn new() -> Self {
        Self {
            lock: Mutex::new(()),
            wake: Condvar::new(),
            sleepers: AtomicUsize::new(0),
        }
    }

    /// Sleeps until woken, unless `must_stay_awake()` is already true. Spurious wake-ups happen; the caller
    /// looks for work again either way.
    pub(crate) fn wait_unless(&self, must_stay_awake: impl FnOnce() -> bool) {
        let mut guard = lock(&self.lock);
        self.sleepers.fetch_add(1, Ordering::SeqCst);
        if !must_stay_awake() {
            guard = self
                .wake
                .wait(guard)
                .unwrap_or_else(PoisonError::into_inner);
        }
        self.sleepers.fetch_sub(1, Ordering::SeqCst);
        drop(guard);
    }

    /// Wakes one sleeper, if there is one.
    pub(crate) fn wake_one(&self) {
        if self.sleepers.load(Ordering::SeqCst) > 0 {
            let _guard = lock(&self.lock);
            self.wake.notify_one();
        }
    }

    /// Wakes every sleeper (shutdown). Always takes the lock, so a worker that is just about to sleep sees the
    /// shutdown flag, which the caller has set before.
    pub(crate) fn wake_all(&self) {
        let _guard = lock(&self.lock);
        self.wake.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::thread;
    use std::time::Duration;

    use super::*;

    #[test]
    fn waiting_returns_at_once_when_the_caller_must_stay_awake() {
        let sleep = Sleep::new();
        sleep.wait_unless(|| true);
        assert_eq!(sleep.sleepers.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_sleeper_is_woken_by_wake_one() {
        let sleep = Arc::new(Sleep::new());
        let flag = Arc::new(AtomicBool::new(false));
        let sleeper = {
            let (sleep, flag) = (Arc::clone(&sleep), Arc::clone(&flag));
            thread::spawn(move || {
                while !flag.load(Ordering::SeqCst) {
                    sleep.wait_unless(|| flag.load(Ordering::SeqCst));
                }
            })
        };
        // Give the thread time to fall asleep (the test is correct without it, just less interesting).
        thread::sleep(Duration::from_millis(30));
        flag.store(true, Ordering::SeqCst);
        sleep.wake_one();
        sleeper.join().unwrap();
    }

    #[test]
    fn wake_one_without_sleepers_does_nothing() {
        let sleep = Sleep::new();
        sleep.wake_one();
        sleep.wake_all();
    }
}
