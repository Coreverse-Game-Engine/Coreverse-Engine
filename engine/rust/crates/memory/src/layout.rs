//! Checked construction of [`Layout`]s from sizes and counts that come from outside.
//!
//! `Layout::from_size_align` and friends are fine for values the program computed itself; sizes read from files
//! or the network need every step checked. These helpers never panic and never wrap.

use core::alloc::Layout;

use crate::align::Alignment;
use crate::error::{MemoryError, MemoryResult};

/// Builds a [`Layout`] from a raw size and alignment.
///
/// ```
/// use memory::{layout_from_parts, MemoryError};
///
/// let layout = layout_from_parts(64, 16)?;
/// assert_eq!((layout.size(), layout.align()), (64, 16));
/// assert_eq!(layout_from_parts(8, 3), Err(MemoryError::InvalidAlignment { align: 3 }));
/// # Ok::<(), MemoryError>(())
/// ```
///
/// # Errors
///
/// * [`MemoryError::InvalidAlignment`] when `align` is not a power of two.
/// * [`MemoryError::SizeOverflow`] when `size`, rounded up to `align`, is larger than `isize::MAX`.
pub fn layout_from_parts(size: usize, align: usize) -> MemoryResult<Layout> {
    let alignment = Alignment::new(align)?;
    Layout::from_size_align(size, alignment.get()).map_err(|_| MemoryError::SizeOverflow)
}

/// The layout of `count` values of the layout `element`, laid out one after another like a Rust array: every
/// element starts at a multiple of its alignment, so the distance between two elements is the element's size
/// rounded up to its alignment.
///
/// ```
/// use core::alloc::Layout;
/// use memory::array_layout;
///
/// // A 5-byte element with alignment 4 takes 8 bytes per slot.
/// let element = Layout::from_size_align(5, 4)?;
/// let array = array_layout(element, 3)?;
/// assert_eq!((array.size(), array.align()), (24, 4));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
///
/// # Errors
///
/// [`MemoryError::SizeOverflow`] when the total size overflows `usize` or is larger than `isize::MAX`.
pub fn array_layout(element: Layout, count: usize) -> MemoryResult<Layout> {
    let stride = element.pad_to_align().size();
    let total = stride.checked_mul(count).ok_or(MemoryError::SizeOverflow)?;
    Layout::from_size_align(total, element.align()).map_err(|_| MemoryError::SizeOverflow)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAX_SIZE: usize = usize::MAX / 2; // isize::MAX

    #[test]
    fn from_parts_accepts_valid_input() {
        let layout = layout_from_parts(16, 8).unwrap();
        assert_eq!((layout.size(), layout.align()), (16, 8));

        let empty = layout_from_parts(0, 1).unwrap();
        assert_eq!((empty.size(), empty.align()), (0, 1));

        let biggest = layout_from_parts(MAX_SIZE, 1).unwrap();
        assert_eq!(biggest.size(), MAX_SIZE);
    }

    #[test]
    fn from_parts_rejects_bad_alignments() {
        for align in [0usize, 3, 6, 24, usize::MAX] {
            assert_eq!(
                layout_from_parts(16, align),
                Err(MemoryError::InvalidAlignment { align }),
                "{align}"
            );
        }
    }

    #[test]
    fn from_parts_rejects_sizes_that_exceed_isize_max() {
        assert_eq!(
            layout_from_parts(MAX_SIZE + 1, 1),
            Err(MemoryError::SizeOverflow)
        );
        assert_eq!(
            layout_from_parts(usize::MAX, 1),
            Err(MemoryError::SizeOverflow)
        );
        // Rounding the size up to the alignment is what pushes it over.
        assert_eq!(
            layout_from_parts(MAX_SIZE, 8),
            Err(MemoryError::SizeOverflow)
        );
    }

    #[test]
    fn array_of_a_plain_type_matches_the_standard_library() {
        let element = Layout::new::<[u32; 3]>(); // size 12, align 4
        let array = array_layout(element, 5).unwrap();
        assert_eq!(array, Layout::array::<[u32; 3]>(5).unwrap());
        assert_eq!((array.size(), array.align()), (60, 4));
    }

    #[test]
    fn array_pads_each_element_to_its_alignment() {
        let element = Layout::from_size_align(5, 4).unwrap();
        let array = array_layout(element, 2).unwrap();
        assert_eq!((array.size(), array.align()), (16, 4));
    }

    #[test]
    fn empty_arrays_have_size_zero_and_keep_the_alignment() {
        let element = Layout::from_size_align(24, 8).unwrap();
        let array = array_layout(element, 0).unwrap();
        assert_eq!((array.size(), array.align()), (0, 8));
    }

    #[test]
    fn array_of_zero_sized_elements_is_empty_for_any_count() {
        let element = Layout::from_size_align(0, 16).unwrap();
        let array = array_layout(element, usize::MAX).unwrap();
        assert_eq!((array.size(), array.align()), (0, 16));
    }

    #[test]
    fn array_reports_overflow_and_oversize() {
        let eight = Layout::new::<u64>();
        assert_eq!(
            array_layout(eight, usize::MAX),
            Err(MemoryError::SizeOverflow)
        );

        // 2^60 elements of 8 bytes are 2^63 bytes: one more than isize::MAX.
        assert_eq!(
            array_layout(eight, MAX_SIZE / 8 + 1),
            Err(MemoryError::SizeOverflow)
        );

        // The largest count that still fits.
        let fits = array_layout(eight, MAX_SIZE / 8).unwrap();
        assert_eq!(fits.size(), MAX_SIZE / 8 * 8);
    }
}
