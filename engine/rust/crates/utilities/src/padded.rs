//! A wrapper that keeps a value on a cache line of its own.

use std::ops::{Deref, DerefMut};

/// Pads and aligns a value to 128 bytes, so that it never shares a cache line with its neighbours.
///
/// Two threads that write to different variables on the same cache line still slow each other down, because the
/// line moves between their cores ("false sharing"). Put each hot, separately written value (a per-worker
/// counter, the head and the tail of a queue) in its own `CachePadded`. 128 bytes covers the 64-byte lines of
/// most x86 and ARM cores and the 128-byte lines of Apple's chips and of the x86 adjacent-line prefetcher.
///
/// The wrapper derefs to the value, so it can be used in place of it.
///
/// ```
/// use std::sync::atomic::{AtomicUsize, Ordering};
///
/// use utilities::CachePadded;
///
/// let counter = CachePadded::new(AtomicUsize::new(0));
/// counter.fetch_add(1, Ordering::Relaxed);
/// assert_eq!(counter.load(Ordering::Relaxed), 1);
/// assert_eq!(std::mem::align_of_val(&counter), 128);
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(align(128))]
pub struct CachePadded<T> {
    value: T,
}

impl<T> CachePadded<T> {
    /// Wraps `value`.
    pub const fn new(value: T) -> Self {
        Self { value }
    }

    /// Takes the value back out.
    pub fn into_inner(self) -> T {
        self.value
    }
}

impl<T> Deref for CachePadded<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.value
    }
}

impl<T> DerefMut for CachePadded<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.value
    }
}

impl<T> From<T> for CachePadded<T> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

#[cfg(test)]
mod tests {
    use std::mem::{align_of, size_of};

    use super::*;

    #[test]
    fn is_aligned_and_sized_to_a_whole_number_of_lines() {
        assert_eq!(align_of::<CachePadded<u8>>(), 128);
        assert_eq!(size_of::<CachePadded<u8>>(), 128);
        assert_eq!(size_of::<CachePadded<[u8; 129]>>(), 256);
    }

    #[test]
    fn neighbours_do_not_share_a_line() {
        let pair = [CachePadded::new(1_u32), CachePadded::new(2_u32)];
        let first = std::ptr::from_ref(&pair[0]) as usize;
        let second = std::ptr::from_ref(&pair[1]) as usize;
        assert_eq!(first % 128, 0);
        assert!(second - first >= 128);
    }

    #[test]
    fn derefs_to_the_value() {
        let mut padded = CachePadded::new(vec![1, 2]);
        padded.push(3);
        assert_eq!(padded.len(), 3);
        assert_eq!(padded.into_inner(), [1, 2, 3]);
    }

    #[test]
    fn converts_from_a_value_and_has_a_default() {
        let padded: CachePadded<u32> = 7.into();
        assert_eq!(*padded, 7);
        assert_eq!(*CachePadded::<u32>::default(), 0);
    }

    #[test]
    fn can_be_made_in_a_const() {
        const PADDED: CachePadded<u32> = CachePadded::new(9);
        assert_eq!(*PADDED, 9);
    }
}
