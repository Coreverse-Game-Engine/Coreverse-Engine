//! Allocation counters and the snapshot type they produce.

use core::fmt;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::size::ByteSize;

/// A copy of a set of counters at one moment.
///
/// The counters are read one after the other while other threads may be allocating, so a snapshot taken under load
/// is approximately consistent (for example `current` can be a few bytes off from `allocations` and
/// `deallocations`); taken while nothing allocates it is exact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MemoryStats {
    /// The bytes in use now.
    pub current: ByteSize,
    /// The highest value `current` has reached (since the counters were created or [`reset`](crate::MemoryTracker::reset_peaks)).
    pub peak: ByteSize,
    /// The sum of all bytes ever allocated; it only grows.
    pub total_allocated: ByteSize,
    /// The number of allocations.
    pub allocations: u64,
    /// The number of deallocations.
    pub deallocations: u64,
    /// The number of reallocations (a resize of an existing block).
    pub reallocations: u64,
}

impl MemoryStats {
    /// The allocations that have not been freed yet.
    pub const fn live_allocations(&self) -> u64 {
        self.allocations.saturating_sub(self.deallocations)
    }
}

impl fmt::Display for MemoryStats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let current = self.current;
        let peak = self.peak;
        let allocations = self.allocations;
        let live = self.live_allocations();
        write!(
            f,
            "{current} in use (peak {peak}), allocations: {allocations} total, {live} live"
        )
    }
}

/// Lock-free counters. Every update is a relaxed atomic operation: the counters are statistics, they order nothing.
///
/// This is also what the [`TrackingAllocator`](crate::TrackingAllocator) updates from inside the global allocator,
/// so nothing here may allocate, lock or panic.
#[derive(Debug)]
pub(crate) struct Counters {
    current: AtomicU64,
    peak: AtomicU64,
    total_allocated: AtomicU64,
    allocations: AtomicU64,
    deallocations: AtomicU64,
    reallocations: AtomicU64,
}

impl Counters {
    pub(crate) const fn new() -> Self {
        Self {
            current: AtomicU64::new(0),
            peak: AtomicU64::new(0),
            total_allocated: AtomicU64::new(0),
            allocations: AtomicU64::new(0),
            deallocations: AtomicU64::new(0),
            reallocations: AtomicU64::new(0),
        }
    }

    pub(crate) fn record_alloc(&self, size: usize) {
        self.grow(size as u64);
        self.allocations.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_dealloc(&self, size: usize) {
        self.shrink(size as u64);
        self.deallocations.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_realloc(&self, old_size: usize, new_size: usize) {
        let (old_size, new_size) = (old_size as u64, new_size as u64);
        if new_size > old_size {
            self.grow(new_size - old_size);
        } else {
            self.shrink(old_size - new_size);
        }
        self.reallocations.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn snapshot(&self) -> MemoryStats {
        MemoryStats {
            current: ByteSize::from_bytes(self.current.load(Ordering::Relaxed)),
            peak: ByteSize::from_bytes(self.peak.load(Ordering::Relaxed)),
            total_allocated: ByteSize::from_bytes(self.total_allocated.load(Ordering::Relaxed)),
            allocations: self.allocations.load(Ordering::Relaxed),
            deallocations: self.deallocations.load(Ordering::Relaxed),
            reallocations: self.reallocations.load(Ordering::Relaxed),
        }
    }

    /// Sets the peak to the current value.
    pub(crate) fn reset_peak(&self) {
        let current = self.current.load(Ordering::Relaxed);
        self.peak.store(current, Ordering::Relaxed);
    }

    fn grow(&self, size: u64) {
        let previous = self.current.fetch_add(size, Ordering::Relaxed);
        let now = previous.saturating_add(size);
        self.peak.fetch_max(now, Ordering::Relaxed);
        self.total_allocated.fetch_add(size, Ordering::Relaxed);
    }

    /// Never goes below zero: freeing memory that was allocated before the counters existed must not wrap.
    ///
    /// A compare-and-swap loop instead of `AtomicU64::fetch_update`/`try_update`: the first is deprecated in newer
    /// toolchains and the second needs Rust 1.95, while the crate's minimum is 1.90.
    fn shrink(&self, size: u64) {
        let relaxed = Ordering::Relaxed;
        let cell = &self.current;
        let mut observed = cell.load(relaxed);
        loop {
            let wanted = observed.saturating_sub(size);
            match cell.compare_exchange_weak(observed, wanted, relaxed, relaxed) {
                Ok(_) => break,
                Err(actual) => observed = actual,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_allocations_and_tracks_the_peak() {
        let counters = Counters::new();
        counters.record_alloc(100);
        counters.record_alloc(50);
        counters.record_dealloc(100);

        let stats = counters.snapshot();
        assert_eq!(stats.current.bytes(), 50);
        assert_eq!(stats.peak.bytes(), 150);
        assert_eq!(stats.total_allocated.bytes(), 150);
        assert_eq!((stats.allocations, stats.deallocations), (2, 1));
        assert_eq!(stats.live_allocations(), 1);
    }

    #[test]
    fn reallocation_changes_the_size_by_the_difference() {
        let counters = Counters::new();
        counters.record_alloc(64);
        counters.record_realloc(64, 256);
        let grown = counters.snapshot();
        assert_eq!(grown.current.bytes(), 256);
        assert_eq!(grown.total_allocated.bytes(), 256);

        counters.record_realloc(256, 32);
        let shrunk = counters.snapshot();
        assert_eq!(shrunk.current.bytes(), 32);
        assert_eq!(shrunk.peak.bytes(), 256);
        assert_eq!(shrunk.reallocations, 2);
        // A resize is neither a new allocation nor a free.
        assert_eq!((shrunk.allocations, shrunk.deallocations), (1, 0));
    }

    #[test]
    fn freeing_more_than_was_counted_stops_at_zero() {
        let counters = Counters::new();
        counters.record_alloc(10);
        counters.record_dealloc(500);
        assert_eq!(counters.snapshot().current.bytes(), 0);
    }

    #[test]
    fn reset_peak_lowers_the_peak_to_the_current_value() {
        let counters = Counters::new();
        counters.record_alloc(300);
        counters.record_dealloc(200);
        counters.reset_peak();
        let stats = counters.snapshot();
        assert_eq!((stats.current.bytes(), stats.peak.bytes()), (100, 100));
    }

    #[test]
    fn many_threads_lose_no_update() {
        let counters = Counters::new();
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    for _ in 0..1000 {
                        counters.record_alloc(16);
                        counters.record_dealloc(16);
                    }
                });
            }
        });
        let stats = counters.snapshot();
        assert_eq!(stats.current.bytes(), 0);
        assert_eq!((stats.allocations, stats.deallocations), (8000, 8000));
        assert_eq!(stats.total_allocated.bytes(), 8000 * 16);
        assert!(stats.peak.bytes() >= 16);
    }

    #[test]
    fn stats_print_readably() {
        let stats = MemoryStats {
            current: ByteSize::from_bytes(1536),
            peak: ByteSize::from_kib(2),
            total_allocated: ByteSize::from_kib(5),
            allocations: 3,
            deallocations: 1,
            reallocations: 0,
        };
        assert_eq!(
            stats.to_string(),
            "1.5 KiB in use (peak 2.0 KiB), allocations: 3 total, 2 live"
        );
    }
}
