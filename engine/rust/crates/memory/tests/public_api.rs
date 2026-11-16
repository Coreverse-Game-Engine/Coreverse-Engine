//! Public API of the `memory` crate: everything is reachable from the crate root and the pieces fit together.

use core::alloc::Layout;

use memory::{Alignment, ByteSize, MemoryError, array_layout, layout_from_parts};

#[test]
fn a_block_can_be_planned_from_untrusted_numbers() {
    // Pretend these two numbers were read from a file header.
    let (count, element_size, element_align) = (100usize, 12usize, 4usize);

    let element = layout_from_parts(element_size, element_align).unwrap();
    let block = array_layout(element, count).unwrap();

    assert_eq!(block.size(), 1200);
    assert_eq!(Alignment::of_layout(block).get(), 4);
    assert_eq!(ByteSize::from(block.size()).to_string(), "1.2 KiB");
}

#[test]
fn hostile_numbers_become_errors_instead_of_panics() {
    assert_eq!(
        layout_from_parts(16, 0),
        Err(MemoryError::InvalidAlignment { align: 0 })
    );

    let element = Layout::new::<u64>();
    assert_eq!(
        array_layout(element, usize::MAX),
        Err(MemoryError::SizeOverflow)
    );
}

#[test]
fn bump_style_placement_arithmetic_works_with_the_public_types() {
    // The core step of a bump allocator: place the next value at the next aligned offset.
    let align = Alignment::of::<u64>();
    let mut offset = 0usize;
    for size in [1usize, 8, 3, 16] {
        offset = align.align_up(offset).unwrap();
        assert!(align.is_aligned(offset));
        offset += size;
    }
    // Values start at 0, 8, 16 and 24; the last one (16 bytes) ends at 40.
    assert_eq!(offset, 40);
}
