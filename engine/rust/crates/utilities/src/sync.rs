//! The standard lock operations with poisoning ignored.
//!
//! A lock is poisoned when a thread panics while it holds the guard. The standard library then makes every later
//! `lock()` return an error, so one failure spreads to every thread that touches the lock. That is the right
//! default for data that can be left half-updated. It is the wrong one for state that stays consistent at every
//! point where the code under the lock can panic, which is how the engine's pools, buses and sinks are written:
//! there, taking the data back keeps one failure from taking the whole system down.
//!
//! Use these functions only for such locks. If a panic under the lock could leave the data broken, use the
//! standard methods and handle the error.

use std::sync::{Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

/// Locks a mutex and ignores poisoning.
///
/// ```
/// use std::sync::Mutex;
///
/// use utilities::sync::lock;
///
/// let counter = Mutex::new(0);
/// *lock(&counter) += 1;
/// assert_eq!(*lock(&counter), 1);
/// ```
pub fn lock<T: ?Sized>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Locks a read-write lock for reading and ignores poisoning.
pub fn read<T: ?Sized>(rw_lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    rw_lock.read().unwrap_or_else(PoisonError::into_inner)
}

/// Locks a read-write lock for writing and ignores poisoning.
pub fn write<T: ?Sized>(rw_lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    rw_lock.write().unwrap_or_else(PoisonError::into_inner)
}

/// Takes the value out of a mutex and ignores poisoning.
///
/// ```
/// use std::sync::Mutex;
///
/// use utilities::sync::{into_inner, lock};
///
/// let names = Mutex::new(vec!["a"]);
/// lock(&names).push("b");
/// assert_eq!(into_inner(names), ["a", "b"]);
/// ```
pub fn into_inner<T>(mutex: Mutex<T>) -> T {
    mutex.into_inner().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::thread;

    use super::*;

    /// A mutex whose owner panicked while holding the guard.
    fn poisoned_mutex() -> Arc<Mutex<i32>> {
        let mutex = Arc::new(Mutex::new(1));
        let other = Arc::clone(&mutex);
        let result = thread::spawn(move || {
            let _guard = other.lock().unwrap();
            panic!("poisoning the mutex on purpose");
        })
        .join();
        assert!(result.is_err());
        assert!(mutex.is_poisoned());
        mutex
    }

    /// A read-write lock whose writer panicked while holding the guard.
    fn poisoned_rw_lock() -> Arc<RwLock<i32>> {
        let rw_lock = Arc::new(RwLock::new(1));
        let other = Arc::clone(&rw_lock);
        let result = thread::spawn(move || {
            let _guard = other.write().unwrap();
            panic!("poisoning the lock on purpose");
        })
        .join();
        assert!(result.is_err());
        assert!(rw_lock.is_poisoned());
        rw_lock
    }

    #[test]
    fn lock_works_on_a_healthy_mutex() {
        let mutex = Mutex::new(5);
        *lock(&mutex) += 1;
        assert_eq!(*lock(&mutex), 6);
    }

    #[test]
    fn lock_ignores_poisoning() {
        let mutex = poisoned_mutex();
        assert_eq!(*lock(&mutex), 1);
        *lock(&mutex) = 2;
        assert_eq!(*lock(&mutex), 2);
    }

    #[test]
    fn into_inner_ignores_poisoning() {
        let mutex = poisoned_mutex();
        let mutex = Arc::try_unwrap(mutex).expect("the panicked thread dropped its clone");
        assert_eq!(into_inner(mutex), 1);
    }

    #[test]
    fn read_and_write_work_on_a_healthy_lock() {
        let rw_lock = RwLock::new(String::from("a"));
        write(&rw_lock).push('b');
        assert_eq!(*read(&rw_lock), "ab");
    }

    #[test]
    fn read_and_write_ignore_poisoning() {
        let rw_lock = poisoned_rw_lock();
        assert_eq!(*read(&rw_lock), 1);
        *write(&rw_lock) = 3;
        assert_eq!(*read(&rw_lock), 3);
    }

    #[test]
    fn lock_accepts_unsized_contents() {
        let mutex: &Mutex<[i32]> = &Mutex::new([1, 2, 3]);
        assert_eq!(lock(mutex).len(), 3);
    }
}
