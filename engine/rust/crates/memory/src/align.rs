//! Power-of-two alignments and the arithmetic arenas and pools need to place data.

use core::alloc::Layout;
use core::fmt;

use crate::error::{MemoryError, MemoryResult};

/// A memory alignment: a power of two, at least 1.
///
/// The invariant is checked once, in [`Alignment::new`]; every other method relies on it, so they cannot fail on
/// a bad alignment. It is the stable counterpart of the unstable `core::ptr::Alignment`.
///
/// ```
/// use memory::Alignment;
///
/// let sixteen = Alignment::new(16)?;
/// assert_eq!(sixteen.align_up(17)?, 32);
/// assert_eq!(sixteen.align_down(17), 16);
/// assert!(sixteen.is_aligned(48));
/// assert!(Alignment::new(12).is_err());
/// # Ok::<(), memory::MemoryError>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Alignment(usize);

impl Alignment {
    /// Alignment 1: no requirement.
    pub const ONE: Self = Self(1);

    /// 64 bytes, the cache-line size of most x86-64 and ARM cores. Some cores use 128-byte lines (Apple silicon,
    /// for instance); 64 is the usual choice to avoid false sharing and is not a guarantee for every CPU.
    pub const CACHE_LINE: Self = Self(64);

    /// Checks that `align` is a power of two.
    ///
    /// # Errors
    ///
    /// [`MemoryError::InvalidAlignment`] for zero and for every value that is not a power of two.
    pub const fn new(align: usize) -> MemoryResult<Self> {
        if align.is_power_of_two() {
            Ok(Self(align))
        } else {
            Err(MemoryError::InvalidAlignment { align })
        }
    }

    /// The alignment of the type `T`.
    pub const fn of<T>() -> Self {
        Self(std::mem::align_of::<T>())
    }

    /// The alignment of a [`Layout`] (always valid: `Layout` guarantees a power of two).
    pub const fn of_layout(layout: Layout) -> Self {
        Self(layout.align())
    }

    /// The alignment in bytes.
    pub const fn get(self) -> usize {
        self.0
    }

    /// The largest multiple of the alignment that is not greater than `value`.
    pub const fn align_down(self, value: usize) -> usize {
        value & !(self.0 - 1)
    }

    /// The smallest multiple of the alignment that is not smaller than `value`.
    ///
    /// # Errors
    ///
    /// [`MemoryError::SizeOverflow`] when that multiple does not fit in `usize`.
    pub const fn align_up(self, value: usize) -> MemoryResult<usize> {
        match value.checked_add(self.0 - 1) {
            Some(sum) => Ok(sum & !(self.0 - 1)),
            None => Err(MemoryError::SizeOverflow),
        }
    }

    /// Whether `value` is a multiple of the alignment.
    pub const fn is_aligned(self, value: usize) -> bool {
        value & (self.0 - 1) == 0
    }

    /// Whether the address of `ptr` is a multiple of the alignment. Only the address is looked at; the pointer is
    /// never dereferenced, so it may be dangling or null (null is aligned to everything).
    pub fn is_ptr_aligned<T: ?Sized>(self, ptr: *const T) -> bool {
        self.is_aligned(ptr.addr())
    }

    /// The number of bytes to skip from `value` to reach the next multiple of the alignment (0 when `value` is
    /// aligned already).
    ///
    /// # Errors
    ///
    /// [`MemoryError::SizeOverflow`] when the next multiple does not fit in `usize`.
    pub fn padding_for(self, value: usize) -> MemoryResult<usize> {
        Ok(self.align_up(value)? - value)
    }
}

impl fmt::Display for Alignment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_every_power_of_two() {
        for shift in 0..usize::BITS {
            let align = 1usize << shift;
            assert_eq!(Alignment::new(align).map(Alignment::get), Ok(align));
        }
    }

    #[test]
    fn rejects_zero_and_non_powers_of_two() {
        for align in [0usize, 3, 5, 6, 12, 100, usize::MAX, usize::MAX / 2] {
            assert_eq!(
                Alignment::new(align),
                Err(MemoryError::InvalidAlignment { align }),
                "{align}"
            );
        }
    }

    #[test]
    fn built_from_types_and_layouts() {
        const U32: Alignment = Alignment::of::<u32>();
        assert_eq!(U32.get(), std::mem::align_of::<u32>());
        assert_eq!(Alignment::of::<u8>(), Alignment::ONE);
        assert_eq!(Alignment::of::<u64>().get(), std::mem::align_of::<u64>());

        let layout = Layout::from_size_align(24, 8).unwrap();
        assert_eq!(Alignment::of_layout(layout).get(), 8);
    }

    #[test]
    fn cache_line_is_a_valid_alignment() {
        assert_eq!(Alignment::new(64), Ok(Alignment::CACHE_LINE));
        assert_eq!(Alignment::CACHE_LINE.get(), 64);
    }

    #[test]
    fn align_up_down_and_padding_on_known_values() {
        let eight = Alignment::new(8).unwrap();
        assert_eq!(eight.align_up(0), Ok(0));
        assert_eq!(eight.align_up(1), Ok(8));
        assert_eq!(eight.align_up(8), Ok(8));
        assert_eq!(eight.align_up(9), Ok(16));
        assert_eq!(eight.align_down(0), 0);
        assert_eq!(eight.align_down(7), 0);
        assert_eq!(eight.align_down(15), 8);
        assert_eq!(eight.align_down(16), 16);
        assert_eq!(eight.padding_for(13), Ok(3));
        assert_eq!(eight.padding_for(16), Ok(0));
    }

    #[test]
    fn alignment_one_changes_nothing() {
        assert_eq!(Alignment::ONE.align_up(12345), Ok(12345));
        assert_eq!(Alignment::ONE.align_down(12345), 12345);
        assert!(Alignment::ONE.is_aligned(12345));
        assert_eq!(Alignment::ONE.padding_for(12345), Ok(0));
        // The largest value does not overflow when nothing has to be added.
        assert_eq!(Alignment::ONE.align_up(usize::MAX), Ok(usize::MAX));
    }

    #[test]
    fn align_up_reports_overflow() {
        let eight = Alignment::new(8).unwrap();
        assert_eq!(eight.align_up(usize::MAX), Err(MemoryError::SizeOverflow));
        assert_eq!(
            eight.align_up(usize::MAX - 6),
            Err(MemoryError::SizeOverflow)
        );
        assert_eq!(
            eight.padding_for(usize::MAX),
            Err(MemoryError::SizeOverflow)
        );
        // The last multiple of 8 that fits is still reachable.
        assert_eq!(eight.align_up(usize::MAX - 7), Ok(usize::MAX - 7));
    }

    #[test]
    fn align_up_and_down_agree_with_the_definition() {
        for shift in 0..13u32 {
            let alignment = Alignment::new(1usize << shift).unwrap();
            let align = alignment.get();
            for value in 0..2048usize {
                let up = alignment.align_up(value).unwrap();
                let down = alignment.align_down(value);
                assert!(alignment.is_aligned(up));
                assert!(alignment.is_aligned(down));
                assert!(up >= value && up - value < align);
                assert!(down <= value && value - down < align);
                assert_eq!(alignment.padding_for(value), Ok(up - value));
                assert_eq!(alignment.is_aligned(value), up == value);
            }
        }
    }

    #[test]
    fn pointer_alignment_looks_at_the_address_only() {
        let value = 0u64;
        assert!(Alignment::of::<u64>().is_ptr_aligned(std::ptr::from_ref(&value)));

        let bytes = [0u8; 4];
        assert!(Alignment::ONE.is_ptr_aligned(std::ptr::from_ref(bytes.as_slice())));

        assert!(Alignment::CACHE_LINE.is_ptr_aligned(std::ptr::null::<u8>()));
        let misaligned = std::ptr::null::<u8>().wrapping_add(1);
        assert!(!Alignment::of::<u16>().is_ptr_aligned(misaligned));
        assert!(Alignment::ONE.is_ptr_aligned(misaligned));
    }

    #[test]
    fn orders_and_prints_by_value() {
        let four = Alignment::new(4).unwrap();
        let sixteen = Alignment::new(16).unwrap();
        assert!(four < sixteen);
        assert_eq!(four.max(sixteen), sixteen);
        assert_eq!(sixteen.to_string(), "16");
    }
}
