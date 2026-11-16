//! A global-allocator wrapper that counts what goes through it.

use core::alloc::Layout;
use std::alloc::{GlobalAlloc, System};

use crate::stats::{Counters, MemoryStats};

/// Wraps another allocator (by default the system's) and counts every allocation, deallocation and reallocation
/// that goes through it: bytes in use, the peak, the total, and the number of calls (see [`MemoryStats`]).
///
/// It is opt-in: nothing in the engine installs it. A binary that wants the numbers declares it as its global
/// allocator:
///
/// ```ignore
/// use memory::TrackingAllocator;
/// use std::alloc::System;
///
/// #[global_allocator]
/// static ALLOCATOR: TrackingAllocator<System> = TrackingAllocator::new(System);
///
/// fn main() {
///     // ... run ...
///     println!("{}", ALLOCATOR.stats());
/// }
/// ```
///
/// (`tests/tracking.rs` in this crate does exactly that and is the compiled version of the example.)
///
/// It sees every Rust allocation of the process, and nothing else: memory that C++ code or the operating system
/// allocates directly does not pass through it. It cannot tell which system an allocation belongs to (the global
/// allocator is not told); for per-system numbers register memory under a [`MemoryTag`](crate::MemoryTag) instead.
///
/// The counting is a handful of relaxed atomic operations per call and never allocates, locks or panics, which a
/// global allocator must not do. The allocator can also be used as a plain value and called directly, without
/// installing it, to measure the allocations of one component.
#[derive(Debug)]
pub struct TrackingAllocator<A = System> {
    inner: A,
    counters: Counters,
}

impl<A> TrackingAllocator<A> {
    /// Wraps `inner`. `const`, so it can initialise a `static`.
    pub const fn new(inner: A) -> Self {
        Self {
            inner,
            counters: Counters::new(),
        }
    }

    /// The counters now.
    pub fn stats(&self) -> MemoryStats {
        self.counters.snapshot()
    }

    /// Sets the peak to the bytes in use now, to measure the peak of one phase.
    pub fn reset_peak(&self) {
        self.counters.reset_peak();
    }

    /// The wrapped allocator.
    pub const fn inner(&self) -> &A {
        &self.inner
    }
}

// SAFETY: every method forwards to the wrapped allocator with the arguments it was given, so the wrapped
// allocator's guarantees (and the caller's obligations) pass through unchanged. The counters are updated only
// after the call, with atomics, and never allocate, lock or panic, as `GlobalAlloc` requires.
unsafe impl<A: GlobalAlloc> GlobalAlloc for TrackingAllocator<A> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller upholds the contract of `GlobalAlloc::alloc` for `layout`.
        let ptr = unsafe { self.inner.alloc(layout) };
        if !ptr.is_null() {
            self.counters.record_alloc(layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: the caller upholds the contract of `GlobalAlloc::dealloc`: `ptr` was allocated by this allocator
        // with `layout`, which is the one forwarded.
        unsafe { self.inner.dealloc(ptr, layout) };
        self.counters.record_dealloc(layout.size());
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller upholds the contract of `GlobalAlloc::alloc_zeroed` for `layout`.
        let ptr = unsafe { self.inner.alloc_zeroed(layout) };
        if !ptr.is_null() {
            self.counters.record_alloc(layout.size());
        }
        ptr
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: the caller upholds the contract of `GlobalAlloc::realloc`: `ptr` was allocated by this allocator
        // with `layout`, and `new_size` is valid for `layout.align()`.
        let new_ptr = unsafe { self.inner.realloc(ptr, layout, new_size) };
        // A failed reallocation (null) leaves the old block untouched, so there is nothing to count.
        if !new_ptr.is_null() {
            self.counters.record_realloc(layout.size(), new_size);
        }
        new_ptr
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An allocator that never has memory.
    struct Exhausted;

    // SAFETY: `alloc` returns null, which `GlobalAlloc` allows to signal failure, and there is never a block to free.
    unsafe impl GlobalAlloc for Exhausted {
        unsafe fn alloc(&self, _layout: Layout) -> *mut u8 {
            core::ptr::null_mut()
        }

        unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {}
    }

    fn layout(size: usize) -> Layout {
        Layout::from_size_align(size, 8).unwrap()
    }

    #[test]
    fn counts_an_allocation_and_its_release() {
        let allocator = TrackingAllocator::new(System);
        let block = layout(256);

        // SAFETY: `block` has a non-zero size; the pointer is freed below with the same layout.
        let ptr = unsafe { allocator.alloc(block) };
        assert!(!ptr.is_null());
        let live = allocator.stats();
        assert_eq!(live.current.bytes(), 256);
        assert_eq!(live.peak.bytes(), 256);
        assert_eq!((live.allocations, live.deallocations), (1, 0));

        // SAFETY: `ptr` came from `allocator.alloc(block)` above and is freed once.
        unsafe { allocator.dealloc(ptr, block) };
        let freed = allocator.stats();
        assert_eq!(freed.current.bytes(), 0);
        assert_eq!(freed.peak.bytes(), 256);
        assert_eq!(freed.total_allocated.bytes(), 256);
        assert_eq!((freed.allocations, freed.deallocations), (1, 1));
    }

    #[test]
    fn zeroed_allocations_are_counted_and_zeroed() {
        let allocator = TrackingAllocator::new(System);
        let block = layout(64);

        // SAFETY: `block` has a non-zero size; the pointer is freed below with the same layout.
        let ptr = unsafe { allocator.alloc_zeroed(block) };
        assert!(!ptr.is_null());
        // SAFETY: the block is 64 bytes long and was just zero-initialised.
        let bytes = unsafe { core::slice::from_raw_parts(ptr, 64) };
        assert!(bytes.iter().all(|byte| *byte == 0));
        assert_eq!(allocator.stats().current.bytes(), 64);

        // SAFETY: `ptr` came from `allocator.alloc_zeroed(block)` and is freed once.
        unsafe { allocator.dealloc(ptr, block) };
        assert_eq!(allocator.stats().current.bytes(), 0);
    }

    #[test]
    fn reallocation_counts_the_difference() {
        let allocator = TrackingAllocator::new(System);
        let small = layout(64);

        // SAFETY: non-zero size; resized and freed below with matching layouts.
        let ptr = unsafe { allocator.alloc(small) };
        assert!(!ptr.is_null());

        // SAFETY: `ptr` was allocated with `small`; 256 is a valid size for alignment 8.
        let ptr = unsafe { allocator.realloc(ptr, small, 256) };
        assert!(!ptr.is_null());
        let grown = allocator.stats();
        assert_eq!(grown.current.bytes(), 256);
        assert_eq!(grown.total_allocated.bytes(), 256);

        let big = layout(256);
        // SAFETY: `ptr` now holds 256 bytes with alignment 8 (`big`); 32 is a valid smaller size.
        let ptr = unsafe { allocator.realloc(ptr, big, 32) };
        assert!(!ptr.is_null());
        let shrunk = allocator.stats();
        assert_eq!(shrunk.current.bytes(), 32);
        assert_eq!(shrunk.peak.bytes(), 256);
        assert_eq!(shrunk.reallocations, 2);
        assert_eq!((shrunk.allocations, shrunk.deallocations), (1, 0));

        // SAFETY: `ptr` holds 32 bytes with alignment 8 and is freed once.
        unsafe { allocator.dealloc(ptr, layout(32)) };
        assert_eq!(allocator.stats().current.bytes(), 0);
    }

    #[test]
    fn a_failed_allocation_counts_nothing() {
        let allocator = TrackingAllocator::new(Exhausted);
        // SAFETY: non-zero size; `Exhausted` returns null and there is nothing to free.
        let ptr = unsafe { allocator.alloc(layout(128)) };
        assert!(ptr.is_null());
        assert_eq!(allocator.stats(), MemoryStats::default());
    }

    #[test]
    fn reset_peak_starts_a_new_measurement() {
        let allocator = TrackingAllocator::new(System);
        let block = layout(512);
        // SAFETY: non-zero size; freed below with the same layout.
        let ptr = unsafe { allocator.alloc(block) };
        // SAFETY: allocated just above with `block`, freed once.
        unsafe { allocator.dealloc(ptr, block) };

        assert_eq!(allocator.stats().peak.bytes(), 512);
        allocator.reset_peak();
        assert_eq!(allocator.stats().peak.bytes(), 0);
    }

    #[test]
    fn can_be_used_from_many_threads() {
        let allocator = TrackingAllocator::new(System);
        let block = layout(64);
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    for _ in 0..1000 {
                        // SAFETY: non-zero size; freed right away with the same layout.
                        unsafe {
                            let ptr = allocator.alloc(block);
                            assert!(!ptr.is_null());
                            allocator.dealloc(ptr, block);
                        }
                    }
                });
            }
        });
        let stats = allocator.stats();
        assert_eq!(stats.current.bytes(), 0);
        assert_eq!((stats.allocations, stats.deallocations), (8000, 8000));
        assert!(stats.peak.bytes() >= 64);
    }

    #[test]
    fn exposes_the_wrapped_allocator() {
        let allocator = TrackingAllocator::new(System);
        let _: &System = allocator.inner();
    }
}
