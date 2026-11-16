//! Arenas through the public API: the patterns an engine system would use them for.

use memory::{Bump, ByteSize, FrameArena, MemoryError};

#[derive(Clone, Copy, Debug, PartialEq)]
struct DrawCall {
    mesh: u32,
    material: u32,
    transform: [f32; 16],
}

#[test]
fn a_frame_loop_reuses_the_same_memory_every_frame() {
    let mut frames = FrameArena::with_capacity(64 * 1024).unwrap();

    for frame in 0..5u32 {
        let calls = frames
            .alloc_slice_fill_with(100, |index| DrawCall {
                mesh: frame,
                material: u32::try_from(index).unwrap(),
                transform: [0.0; 16],
            })
            .unwrap();
        assert_eq!(calls.len(), 100);
        assert_eq!(calls[99].material, 99);
        assert_eq!(calls[0].mesh, frame);
        assert_eq!(calls[0].transform, [0.0; 16]);

        frames.begin_frame();
        assert_eq!(frames.used(), 0);
    }

    // Every frame used the same amount, and the peak shows what to budget for.
    assert_eq!(frames.last_frame_used(), frames.peak());
    assert!(frames.peak() >= 100 * size_of::<DrawCall>());
    assert_eq!(frames.frame(), 5);
}

#[test]
fn a_frame_that_needs_too_much_fails_cleanly() {
    let frames = FrameArena::with_capacity(1024).unwrap();
    let error = frames.alloc_slice_fill(1000, 0u64).unwrap_err();
    assert_eq!(
        error,
        MemoryError::OutOfMemory {
            requested: 8000,
            available: 1024,
        }
    );
    // The arena is untouched and still usable.
    assert!(frames.alloc_slice_fill(100, 0u64).is_ok());
}

#[test]
fn markers_give_stack_style_scratch_space() {
    let mut bump = Bump::with_byte_size(ByteSize::from_kib(4)).unwrap();
    let _persistent = bump.alloc(42u32).unwrap();
    let after_persistent = bump.used();

    for round in 0..10u8 {
        let marker = bump.marker();
        {
            let scratch = bump.alloc_slice_fill(512, round).unwrap();
            assert!(scratch.iter().all(|byte| *byte == round));
        }
        bump.rewind(marker).unwrap();
        assert_eq!(bump.used(), after_persistent);
    }
}
