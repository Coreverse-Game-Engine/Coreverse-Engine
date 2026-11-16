//! Job priorities.

/// How urgent a job is. A job of a higher priority always starts before a queued job of a lower one.
///
/// The order is strict: while `High` jobs are queued, no `Normal` or `Low` job is started, and likewise `Normal`
/// before `Low`. A steady stream of high-priority jobs can therefore starve the lower levels; use `High` for
/// work that is short and must not wait (the next frame's input, a render-thread hand-off) and `Low` for work that
/// may wait for idle time (asset streaming, background compression). A job that is already running is never
/// interrupted by a more urgent one.
///
/// Within one priority, jobs spawned from outside the pool start in the order they were spawned.
///
/// This is the priority of a *job*. The OS priority of the worker *threads* is
/// [`ThreadPriority`](crate::ThreadPriority).
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug, Default)]
pub enum JobPriority {
    /// Background work that should only use time nothing else wants.
    Low,
    /// The default.
    #[default]
    Normal,
    /// Latency-sensitive work that should start as soon as a worker is free.
    High,
}

/// The number of priority levels, and so of lanes in every queue.
pub(crate) const LANES: usize = 3;

impl JobPriority {
    /// Every level, from the least to the most urgent.
    pub const ALL: [Self; LANES] = [Self::Low, Self::Normal, Self::High];

    /// The index of this level's lane in a queue (`Low` = 0, `High` = 2), so a higher index is more urgent.
    pub(crate) fn lane(self) -> usize {
        match self {
            Self::Low => 0,
            Self::Normal => 1,
            Self::High => 2,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_are_ordered_from_low_to_high() {
        assert!(JobPriority::Low < JobPriority::Normal);
        assert!(JobPriority::Normal < JobPriority::High);
        assert_eq!(JobPriority::default(), JobPriority::Normal);
    }

    #[test]
    fn lanes_follow_the_order_and_cover_every_level() {
        let lanes: Vec<usize> = JobPriority::ALL.iter().map(|p| p.lane()).collect();
        assert_eq!(lanes, [0, 1, 2]);
        assert_eq!(JobPriority::ALL.len(), LANES);
    }
}
