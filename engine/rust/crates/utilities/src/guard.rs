//! A scope guard that runs a closure when it is dropped.

use std::fmt;

/// Runs a closure when it is dropped: at the end of the scope, on an early `return` or `?`, and while a panic
/// unwinds. Made by [`defer`](fn@defer). [`cancel`](Self::cancel) disarms it.
///
/// Bind the guard to a name (`let _done = ...`), not to `_`, which would drop it on the spot. Like any code that
/// runs in a destructor, the closure should not panic while another panic is unwinding: that aborts the process.
///
/// ```
/// use utilities::defer;
///
/// let mut steps = Vec::new();
/// {
///     let _cleanup = defer(|| steps.push("cleanup"));
/// }
/// assert_eq!(steps, ["cleanup"]);
/// ```
#[must_use = "the closure runs when the guard is dropped; bind it to a name such as `_guard`"]
pub struct Defer<F: FnOnce()> {
    action: Option<F>,
}

impl<F: FnOnce()> Defer<F> {
    /// Makes a guard that runs `action` when it is dropped. [`defer`] is the same thing.
    pub fn new(action: F) -> Self {
        Self {
            action: Some(action),
        }
    }

    /// Disarms the guard: the closure is dropped without running.
    ///
    /// ```
    /// use utilities::defer;
    ///
    /// let mut ran = false;
    /// let guard = defer(|| ran = true);
    /// guard.cancel();
    /// assert!(!ran);
    /// ```
    pub fn cancel(mut self) {
        self.action = None;
    }
}

impl<F: FnOnce()> Drop for Defer<F> {
    fn drop(&mut self) {
        if let Some(action) = self.action.take() {
            action();
        }
    }
}

impl<F: FnOnce()> fmt::Debug for Defer<F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Defer")
            .field("armed", &self.action.is_some())
            .finish()
    }
}

/// Runs `action` when the returned guard is dropped (see [`Defer`]).
pub fn defer<F: FnOnce()>(action: F) -> Defer<F> {
    Defer::new(action)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    #[test]
    fn runs_when_the_scope_ends() {
        let count = Cell::new(0);
        {
            let _guard = defer(|| count.set(count.get() + 1));
            assert_eq!(count.get(), 0, "nothing runs before the drop");
        }
        assert_eq!(count.get(), 1);
    }

    #[test]
    fn runs_once() {
        let count = Cell::new(0);
        let guard = defer(|| count.set(count.get() + 1));
        drop(guard);
        assert_eq!(count.get(), 1);
    }

    #[test]
    fn cancel_disarms_the_guard() {
        let count = Cell::new(0);
        defer(|| count.set(1)).cancel();
        assert_eq!(count.get(), 0);
    }

    #[test]
    fn runs_on_an_early_return() {
        fn work(count: &Cell<i32>, fail: bool) -> Result<(), ()> {
            let _guard = defer(|| count.set(count.get() + 1));
            if fail {
                return Err(());
            }
            Ok(())
        }
        let count = Cell::new(0);
        assert!(work(&count, true).is_err());
        assert!(work(&count, false).is_ok());
        assert_eq!(count.get(), 2);
    }

    #[test]
    fn runs_while_a_panic_unwinds() {
        let count = Cell::new(0);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = defer(|| count.set(count.get() + 1));
            panic!("unwinding on purpose");
        }));
        assert!(result.is_err());
        assert_eq!(count.get(), 1);
    }

    #[test]
    fn guards_run_in_reverse_order() {
        let order = std::cell::RefCell::new(Vec::new());
        {
            let _first = defer(|| order.borrow_mut().push(1));
            let _second = defer(|| order.borrow_mut().push(2));
        }
        assert_eq!(*order.borrow(), [2, 1]);
    }

    #[test]
    fn debug_shows_whether_the_guard_is_armed() {
        let guard = defer(|| {});
        assert_eq!(format!("{guard:?}"), "Defer { armed: true }");
    }
}
