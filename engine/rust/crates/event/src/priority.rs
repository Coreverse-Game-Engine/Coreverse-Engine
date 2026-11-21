//! The order in which the handlers of one event run.

/// How early a handler runs among the handlers of the same event type.
///
/// Handlers of a higher priority always run before handlers of a lower one. Handlers of the same priority run
/// in the order in which they were subscribed. [`EventPriority::Normal`] is the default.
///
/// ```
/// use event::EventPriority;
///
/// assert!(EventPriority::High > EventPriority::Normal);
/// assert!(EventPriority::Normal > EventPriority::Low);
/// assert_eq!(EventPriority::default(), EventPriority::Normal);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum EventPriority {
    /// Runs after every `Normal` and `High` handler of the same event type.
    Low = 0,
    /// The default priority.
    #[default]
    Normal = 1,
    /// Runs before every `Normal` and `Low` handler of the same event type.
    High = 2,
}

#[cfg(test)]
mod tests {
    use super::EventPriority;

    #[test]
    fn higher_priorities_compare_greater() {
        assert!(EventPriority::High > EventPriority::Normal);
        assert!(EventPriority::Normal > EventPriority::Low);
        assert!(EventPriority::High > EventPriority::Low);
    }

    #[test]
    fn the_default_is_normal() {
        assert_eq!(EventPriority::default(), EventPriority::Normal);
    }
}
