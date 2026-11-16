//! The per-frame arena: a [`Bump`] that is emptied once per frame.

use core::ops::Deref;

use crate::bump::Bump;
use crate::error::MemoryResult;
use crate::tag::MemoryTag;

/// An arena for data that lives for one frame: render lists, scratch buffers, temporary strings.
///
/// It is a [`Bump`] plus a frame counter and the usage of the last frame. Allocate through it exactly as through
/// a `Bump` (`FrameArena` dereferences to one); the one way to empty it is [`FrameArena::begin_frame`], which needs
/// `&mut self`, so no reference into last frame's data can survive into the next frame:
///
/// ```
/// use memory::FrameArena;
///
/// let mut frames = FrameArena::with_capacity(1 << 20)?;
/// for expected_frame in 0..3 {
///     assert_eq!(frames.frame(), expected_frame);
///     let scratch = frames.alloc_slice_fill(256, 0.0f32)?;
///     scratch[0] = 1.0;
///     frames.begin_frame();
/// }
/// assert_eq!(frames.last_frame_used(), 1024);
/// # Ok::<(), memory::MemoryError>(())
/// ```
///
/// There is no double buffering (reading frame N's data during frame N + 1): a Rust reference cannot cross
/// `begin_frame`, and handing data between frames needs another design (handles or offsets) that no system needs
/// yet.
#[derive(Debug)]
pub struct FrameArena {
    bump: Bump,
    frame: u64,
    last_frame_used: usize,
}

impl FrameArena {
    /// A frame arena with room for `capacity` bytes per frame, starting at frame 0.
    ///
    /// # Errors
    ///
    /// As [`Bump::with_capacity`].
    pub fn with_capacity(capacity: usize) -> MemoryResult<Self> {
        Ok(Self {
            bump: Bump::with_capacity(capacity)?,
            frame: 0,
            last_frame_used: 0,
        })
    }

    /// Like [`FrameArena::with_capacity`], with the block registered under `tag` in the global tracker (see
    /// [`Bump::with_capacity_tagged`]).
    ///
    /// # Errors
    ///
    /// As [`Bump::with_capacity`].
    pub fn with_capacity_tagged(capacity: usize, tag: MemoryTag) -> MemoryResult<Self> {
        Ok(Self {
            bump: Bump::with_capacity_tagged(capacity, tag)?,
            frame: 0,
            last_frame_used: 0,
        })
    }

    /// Ends the current frame: remembers how much it used, frees everything and starts the next frame.
    pub fn begin_frame(&mut self) {
        self.last_frame_used = self.bump.used();
        self.bump.reset();
        self.frame = self.frame.saturating_add(1);
    }

    /// The number of the current frame: 0 until the first [`FrameArena::begin_frame`].
    pub fn frame(&self) -> u64 {
        self.frame
    }

    /// The bytes the previous frame used (0 before the first `begin_frame`).
    pub fn last_frame_used(&self) -> usize {
        self.last_frame_used
    }
}

impl Deref for FrameArena {
    type Target = Bump;

    /// The underlying [`Bump`], for allocating and for reading the counters. Only shared access is offered: the
    /// arena can be emptied through [`FrameArena::begin_frame`] only.
    fn deref(&self) -> &Bump {
        &self.bump
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::MemoryError;

    #[test]
    fn starts_at_frame_zero_with_nothing_used() {
        let frames = FrameArena::with_capacity(128).unwrap();
        assert_eq!(frames.frame(), 0);
        assert_eq!(frames.last_frame_used(), 0);
        assert_eq!(frames.used(), 0);
        assert_eq!(frames.capacity(), 128);
    }

    #[test]
    fn begin_frame_empties_the_arena_and_counts() {
        let mut frames = FrameArena::with_capacity(64).unwrap();
        let _ = frames.alloc([0u8; 40]).unwrap();
        assert_eq!(frames.used(), 40);

        frames.begin_frame();
        assert_eq!(frames.frame(), 1);
        assert_eq!(frames.used(), 0);
        assert_eq!(frames.last_frame_used(), 40);
        assert_eq!(frames.remaining(), 64);

        let _ = frames.alloc([0u8; 8]).unwrap();
        frames.begin_frame();
        assert_eq!(frames.frame(), 2);
        assert_eq!(frames.last_frame_used(), 8);
    }

    #[test]
    fn the_peak_covers_all_frames() {
        let mut frames = FrameArena::with_capacity(64).unwrap();
        let _ = frames.alloc([0u8; 48]).unwrap();
        frames.begin_frame();
        let _ = frames.alloc([0u8; 8]).unwrap();
        assert_eq!(frames.peak(), 48);
    }

    #[test]
    fn a_full_frame_fails_the_same_way_as_a_bump() {
        let frames = FrameArena::with_capacity(8).unwrap();
        let _ = frames.alloc(1u64).unwrap();
        assert_eq!(
            frames.alloc(1u8).unwrap_err(),
            MemoryError::OutOfMemory {
                requested: 1,
                available: 0,
            }
        );
    }

    #[test]
    fn a_tagged_frame_arena_registers_its_block() {
        // No other unit test of this crate uses `MemoryTag::Scripting`.
        let tag = MemoryTag::Scripting;
        let current = || crate::global_tracker().stats(tag).current.bytes();
        let before = current();

        let mut frames = FrameArena::with_capacity_tagged(2048, tag).unwrap();
        assert_eq!(frames.tag(), Some(tag));
        assert_eq!(current(), before + 2048);

        // Starting a new frame reuses the block: nothing is allocated or freed.
        frames.begin_frame();
        assert_eq!(current(), before + 2048);

        drop(frames);
        assert_eq!(current(), before);
    }

    #[test]
    fn can_move_to_another_thread() {
        fn assert_send<T: Send>() {}
        assert_send::<FrameArena>();
    }
}
