//! Small helpers around the standard synchronization types.

use std::sync::{Mutex, MutexGuard, PoisonError};

/// Locks a mutex and ignores poisoning.
///
/// Job code never runs while one of this crate's locks is held (jobs are wrapped in `catch_unwind` and run
/// outside every lock), so a poisoned lock can only come from a bug in this crate's own few lines under the
/// lock. Those lines leave the protected data consistent at every point where they could panic, so taking the
/// data back is safe and keeps one failure from taking the whole pool down.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
