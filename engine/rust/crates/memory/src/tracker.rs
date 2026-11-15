//! Per-tag accounting for explicitly registered memory, budgets and the report built from them.

use core::fmt;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::size::ByteSize;
use crate::stats::{Counters, MemoryStats};
use crate::tag::MemoryTag;

/// The value stored for "no budget".
const NO_BUDGET: u64 = u64::MAX;

/// Counts memory per [`MemoryTag`], with an optional budget per tag.
///
/// Memory is *registered* explicitly: [`Bump::with_capacity_tagged`](crate::Bump::with_capacity_tagged) and
/// [`FrameArena::with_capacity_tagged`](crate::FrameArena::with_capacity_tagged) record their block in the
/// [`global_tracker`], and any code can call [`MemoryTracker::record_alloc`] and
/// [`MemoryTracker::record_dealloc`] for its own allocations. The tracker does not see allocations that nobody
/// registered; for the process as a whole use the [`TrackingAllocator`](crate::TrackingAllocator).
///
/// All methods take `&self` and use atomics, so one tracker can be shared by every thread. Budgets are advisory:
/// going over one changes nothing but [`MemoryTracker::is_over_budget`] and the [`MemoryReport`].
///
/// ```
/// use memory::{ByteSize, MemoryTag, MemoryTracker};
///
/// let tracker = MemoryTracker::new();
/// tracker.set_budget(MemoryTag::Rendering, Some(ByteSize::from_mib(1)));
///
/// tracker.record_alloc(MemoryTag::Rendering, 3 << 19); // 1.5 MiB
/// assert!(tracker.is_over_budget(MemoryTag::Rendering));
///
/// tracker.record_dealloc(MemoryTag::Rendering, 3 << 19);
/// assert!(!tracker.report().has_leaks());
/// ```
pub struct MemoryTracker {
    total: Counters,
    tags: [Counters; MemoryTag::COUNT],
    budgets: [AtomicU64; MemoryTag::COUNT],
}

impl MemoryTracker {
    /// A tracker with all counters at zero and no budgets. `const`, so it can be a `static`.
    pub const fn new() -> Self {
        Self {
            total: Counters::new(),
            tags: [const { Counters::new() }; MemoryTag::COUNT],
            budgets: [const { AtomicU64::new(NO_BUDGET) }; MemoryTag::COUNT],
        }
    }

    /// Records an allocation of `size` bytes under `tag`.
    pub fn record_alloc(&self, tag: MemoryTag, size: usize) {
        self.tags[tag.index()].record_alloc(size);
        self.total.record_alloc(size);
    }

    /// Records that `size` bytes registered under `tag` were freed.
    pub fn record_dealloc(&self, tag: MemoryTag, size: usize) {
        self.tags[tag.index()].record_dealloc(size);
        self.total.record_dealloc(size);
    }

    /// The counters of one tag.
    pub fn stats(&self, tag: MemoryTag) -> MemoryStats {
        self.tags[tag.index()].snapshot()
    }

    /// The counters of all tags together.
    pub fn total(&self) -> MemoryStats {
        self.total.snapshot()
    }

    /// Sets the budget of a tag, or removes it with `None`.
    pub fn set_budget(&self, tag: MemoryTag, budget: Option<ByteSize>) {
        let value = budget.map_or(NO_BUDGET, ByteSize::bytes);
        self.budgets[tag.index()].store(value, Ordering::Relaxed);
    }

    /// The budget of a tag, if one is set.
    pub fn budget(&self, tag: MemoryTag) -> Option<ByteSize> {
        let value = self.budgets[tag.index()].load(Ordering::Relaxed);
        (value != NO_BUDGET).then_some(ByteSize::from_bytes(value))
    }

    /// Whether the tag has a budget and uses more than it.
    pub fn is_over_budget(&self, tag: MemoryTag) -> bool {
        let current = self.stats(tag).current;
        self.budget(tag).is_some_and(|budget| current > budget)
    }

    /// Sets the peak of every counter to its current value, to measure the peak of one phase (a level, a frame).
    pub fn reset_peaks(&self) {
        self.total.reset_peak();
        for counters in &self.tags {
            counters.reset_peak();
        }
    }

    /// A copy of everything: the total and every tag that has seen an allocation or has a budget.
    pub fn report(&self) -> MemoryReport {
        let tags = MemoryTag::ALL
            .iter()
            .filter_map(|&tag| {
                let snapshot = self.stats(tag);
                let limit = self.budget(tag);
                let used = snapshot.allocations > 0 || limit.is_some();
                used.then_some(TagReport {
                    tag,
                    stats: snapshot,
                    budget: limit,
                })
            })
            .collect();
        MemoryReport {
            total: self.total(),
            tags,
        }
    }
}

impl Default for MemoryTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for MemoryTracker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MemoryTracker")
            .field("total", &self.total())
            .finish_non_exhaustive()
    }
}

/// The process-wide tracker that the `with_capacity_tagged` constructors register their blocks in.
pub fn global_tracker() -> &'static MemoryTracker {
    static TRACKER: MemoryTracker = MemoryTracker::new();
    &TRACKER
}

/// One tag's line of a [`MemoryReport`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TagReport {
    /// The tag.
    pub tag: MemoryTag,
    /// Its counters.
    pub stats: MemoryStats,
    /// Its budget, if one was set.
    pub budget: Option<ByteSize>,
}

impl TagReport {
    /// Whether the tag still has bytes in use.
    pub fn is_leak(&self) -> bool {
        !self.stats.current.is_zero()
    }

    /// Whether the tag has a budget and uses more than it.
    pub fn is_over_budget(&self) -> bool {
        self.budget
            .is_some_and(|budget| self.stats.current > budget)
    }
}

impl fmt::Display for TagReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.tag, self.stats)?;
        if let Some(budget) = self.budget {
            write!(f, ", budget {budget}")?;
            if self.is_over_budget() {
                write!(f, " (over budget)")?;
            }
        }
        Ok(())
    }
}

/// A copy of a [`MemoryTracker`]: the total and one [`TagReport`] per tag that was used or has a budget, in tag
/// order. Taken with [`MemoryTracker::report`].
///
/// Call it when the program shuts down (or a level ends): a tag that still has bytes in use then is a *leak*, and
/// [`MemoryReport::leaks`] lists those tags. Turning the report into log messages is up to the caller; it prints
/// as plain text through `Display`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryReport {
    /// All tags together.
    pub total: MemoryStats,
    /// The tags that were used or have a budget.
    pub tags: Vec<TagReport>,
}

impl MemoryReport {
    /// The tags that still have bytes in use.
    pub fn leaks(&self) -> impl Iterator<Item = &TagReport> {
        self.tags.iter().filter(|entry| entry.is_leak())
    }

    /// Whether any tag still has bytes in use.
    pub fn has_leaks(&self) -> bool {
        self.leaks().next().is_some()
    }

    /// The tags that use more than their budget.
    pub fn over_budget(&self) -> impl Iterator<Item = &TagReport> {
        self.tags.iter().filter(|entry| entry.is_over_budget())
    }
}

impl fmt::Display for MemoryReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "total: {}", self.total)?;
        for report in &self.tags {
            write!(f, "\n{report}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_per_tag_and_in_total() {
        let tracker = MemoryTracker::new();
        tracker.record_alloc(MemoryTag::Rendering, 1000);
        tracker.record_alloc(MemoryTag::Assets, 500);

        assert_eq!(tracker.stats(MemoryTag::Rendering).current.bytes(), 1000);
        assert_eq!(tracker.stats(MemoryTag::Assets).current.bytes(), 500);
        assert_eq!(tracker.stats(MemoryTag::Ecs).current.bytes(), 0);
        assert_eq!(tracker.total().current.bytes(), 1500);

        tracker.record_dealloc(MemoryTag::Rendering, 1000);
        assert_eq!(tracker.stats(MemoryTag::Rendering).current.bytes(), 0);
        assert_eq!(tracker.stats(MemoryTag::Rendering).peak.bytes(), 1000);
        assert_eq!(tracker.total().current.bytes(), 500);
        assert_eq!(tracker.total().peak.bytes(), 1500);
    }

    #[test]
    fn budgets_can_be_set_read_and_removed() {
        let tracker = MemoryTracker::new();
        assert_eq!(tracker.budget(MemoryTag::Audio), None);

        tracker.set_budget(MemoryTag::Audio, Some(ByteSize::from_kib(1)));
        assert_eq!(
            tracker.budget(MemoryTag::Audio),
            Some(ByteSize::from_kib(1))
        );
        assert!(!tracker.is_over_budget(MemoryTag::Audio));

        tracker.record_alloc(MemoryTag::Audio, 1024);
        assert!(!tracker.is_over_budget(MemoryTag::Audio)); // exactly at the budget
        tracker.record_alloc(MemoryTag::Audio, 1);
        assert!(tracker.is_over_budget(MemoryTag::Audio));

        tracker.set_budget(MemoryTag::Audio, None);
        assert!(!tracker.is_over_budget(MemoryTag::Audio));
        assert_eq!(tracker.budget(MemoryTag::Audio), None);
    }

    #[test]
    fn a_budget_of_zero_is_a_budget() {
        let tracker = MemoryTracker::new();
        tracker.set_budget(MemoryTag::Ui, Some(ByteSize::ZERO));
        assert_eq!(tracker.budget(MemoryTag::Ui), Some(ByteSize::ZERO));
        tracker.record_alloc(MemoryTag::Ui, 1);
        assert!(tracker.is_over_budget(MemoryTag::Ui));
    }

    #[test]
    fn reset_peaks_starts_a_new_measurement() {
        let tracker = MemoryTracker::new();
        tracker.record_alloc(MemoryTag::Ecs, 800);
        tracker.record_dealloc(MemoryTag::Ecs, 600);
        tracker.reset_peaks();
        assert_eq!(tracker.stats(MemoryTag::Ecs).peak.bytes(), 200);
        assert_eq!(tracker.total().peak.bytes(), 200);
    }

    #[test]
    fn the_report_lists_used_tags_in_tag_order() {
        let tracker = MemoryTracker::new();
        tracker.set_budget(MemoryTag::Rendering, Some(ByteSize::from_kib(1)));
        tracker.record_alloc(MemoryTag::Rendering, 2048);
        tracker.record_alloc(MemoryTag::Assets, 512);
        tracker.record_dealloc(MemoryTag::Assets, 512);
        tracker.set_budget(MemoryTag::Tools, Some(ByteSize::from_mib(1)));

        let report = tracker.report();
        let tags: Vec<MemoryTag> = report.tags.iter().map(|entry| entry.tag).collect();
        assert_eq!(
            tags,
            [MemoryTag::Rendering, MemoryTag::Assets, MemoryTag::Tools]
        );
        assert_eq!(report.total.current.bytes(), 2048);

        let leaks: Vec<MemoryTag> = report.leaks().map(|entry| entry.tag).collect();
        assert_eq!(leaks, [MemoryTag::Rendering]);
        assert!(report.has_leaks());

        let over: Vec<MemoryTag> = report.over_budget().map(|entry| entry.tag).collect();
        assert_eq!(over, [MemoryTag::Rendering]);
    }

    #[test]
    fn an_idle_tracker_has_an_empty_report_without_leaks() {
        let report = MemoryTracker::new().report();
        assert!(report.tags.is_empty());
        assert!(!report.has_leaks());
        assert_eq!(report.over_budget().count(), 0);
    }

    #[test]
    fn the_report_prints_one_line_per_tag() {
        let tracker = MemoryTracker::new();
        tracker.set_budget(MemoryTag::Rendering, Some(ByteSize::from_kib(1)));
        tracker.record_alloc(MemoryTag::Rendering, 2048);
        tracker.record_alloc(MemoryTag::Assets, 512);

        let text = tracker.report().to_string();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3, "{text}");
        assert_eq!(
            lines[0],
            "total: 2.5 KiB in use (peak 2.5 KiB), allocations: 2 total, 2 live"
        );
        assert_eq!(
            lines[1],
            "Rendering: 2.0 KiB in use (peak 2.0 KiB), allocations: 1 total, 1 live, \
             budget 1.0 KiB (over budget)"
        );
        assert_eq!(
            lines[2],
            "Assets: 512 B in use (peak 512 B), allocations: 1 total, 1 live"
        );
    }

    #[test]
    fn the_global_tracker_is_one_shared_instance() {
        assert!(std::ptr::eq(global_tracker(), global_tracker()));
    }

    #[test]
    fn can_be_shared_between_threads() {
        let tracker = MemoryTracker::new();
        std::thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| {
                    for _ in 0..500 {
                        tracker.record_alloc(MemoryTag::Networking, 8);
                        tracker.record_dealloc(MemoryTag::Networking, 8);
                    }
                });
            }
        });
        assert_eq!(tracker.stats(MemoryTag::Networking).current.bytes(), 0);
        assert_eq!(tracker.total().allocations, 2000);
    }

    #[test]
    fn debug_output_shows_the_total() {
        let tracker = MemoryTracker::new();
        tracker.record_alloc(MemoryTag::General, 8);
        let text = format!("{tracker:?}");
        assert!(text.starts_with("MemoryTracker { total: MemoryStats {"));
        assert!(text.ends_with(".. }"));
    }
}
