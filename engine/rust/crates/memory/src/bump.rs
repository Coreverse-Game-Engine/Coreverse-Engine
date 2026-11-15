//! The bump allocator: a fixed block of memory handed out front to back.
//!
//! Allocating is a pointer bump and freeing is not possible one value at a time: everything goes at once with
//! [`Bump::reset`], or back to an earlier point with [`Bump::rewind`]. That makes it the right tool for data with
//! one shared lifetime (a frame, a level load, a temporary computation).
//!
//! # Safety design
//!
//! This is the only module of the crate with `unsafe` code. The invariants it relies on:
//!
//! * `block` owns `capacity` bytes (or nothing, when the capacity is 0) from the system allocator, aligned to at
//!   least [`BLOCK_ALIGN`], and frees them when dropped.
//! * `offset <= capacity` always, and everything below `offset` may have been handed out; everything at or above
//!   it never has been since the last `reset`/`rewind`.
//! * A reference returned by an allocation method borrows the `Bump` (`&self`). `reset` and `rewind` need
//!   `&mut self`, so the borrow checker guarantees that no reference into the old contents is alive when the
//!   memory is reused.
//! * Only values that are `Copy` are stored. They have no destructor, so reusing memory without running one is
//!   not a leak, and a panic in a user closure cannot leave half-dropped values behind.

use core::alloc::Layout;
use core::cell::Cell;
use core::fmt;
use core::ptr::NonNull;

use crate::align::Alignment;
use crate::error::{MemoryError, MemoryResult};
use crate::layout::{array_layout, layout_from_parts};
use crate::size::ByteSize;

/// Alignment of the memory block itself. Requests with a larger alignment are served by aligning the address
/// inside the block, which wastes up to `align - BLOCK_ALIGN` bytes per request.
const BLOCK_ALIGN: usize = 16;

/// An owned heap block, freed on drop.
struct Block {
    ptr: NonNull<u8>,
    layout: Layout,
}

// SAFETY: a `Block` owns its memory exclusively, like a `Box<[u8]>`; nothing else holds a pointer to it, so it can
// move to another thread. (It is not `Sync` as a type on its own because `Bump` adds `Cell`s; the block's memory
// is only reached through `&Bump`/`&mut Bump`.)
unsafe impl Send for Block {}

impl Block {
    fn new(capacity: usize) -> MemoryResult<Self> {
        // Rejects capacities above `isize::MAX`.
        let layout = layout_from_parts(capacity, BLOCK_ALIGN)?;

        if capacity == 0 {
            // Nothing to allocate: a dangling, aligned pointer stands in. No request of size > 0 can succeed, so
            // it is never dereferenced.
            let dangling = std::ptr::without_provenance_mut::<u8>(BLOCK_ALIGN);
            let ptr = NonNull::new(dangling).unwrap_or(NonNull::dangling());
            return Ok(Self { ptr, layout });
        }

        // SAFETY: `layout` has a non-zero size, because `capacity > 0`.
        let raw = unsafe { std::alloc::alloc(layout) };
        match NonNull::new(raw) {
            Some(ptr) => Ok(Self { ptr, layout }),
            None => Err(MemoryError::AllocationFailed { size: capacity }),
        }
    }
}

impl Drop for Block {
    fn drop(&mut self) {
        if self.layout.size() != 0 {
            // SAFETY: `ptr` came from `std::alloc::alloc` with exactly this `layout` (see `Block::new`) and has
            // not been freed; a block of size 0 was never allocated and is skipped above.
            unsafe { std::alloc::dealloc(self.ptr.as_ptr(), self.layout) };
        }
    }
}

/// A position in a [`Bump`], taken with [`Bump::marker`] and used with [`Bump::rewind`] to free everything that
/// was allocated after it (stack-style allocation).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Marker(usize);

/// A fixed-capacity bump allocator for `Copy` values.
///
/// Every allocation method takes `&self` and returns a reference that borrows the allocator, so many values can
/// be alive at once. Freeing takes `&mut self` ([`reset`](Bump::reset), [`rewind`](Bump::rewind)), so the
/// compiler rejects any use of a value after its memory was given back:
///
/// ```compile_fail
/// use memory::Bump;
///
/// let mut bump = Bump::with_capacity(64).unwrap();
/// let value = bump.alloc(1u32).unwrap();
/// bump.reset(); // error: `bump` is still borrowed by `value`
/// *value += 1;
/// ```
///
/// Used the intended way:
///
/// ```
/// use memory::Bump;
///
/// let mut bump = Bump::with_capacity(1024)?;
/// {
///     let position = bump.alloc([1.0f32, 2.0, 3.0])?;
///     let indices = bump.alloc_slice_copy(&[0u16, 1, 2])?;
///     position[0] = 10.0;
///     assert_eq!((position[0], indices[2]), (10.0, 2));
/// }
/// assert!(bump.used() > 0);
/// bump.reset();
/// assert_eq!(bump.used(), 0);
/// # Ok::<(), memory::MemoryError>(())
/// ```
///
/// The capacity is fixed: a request that does not fit fails with [`MemoryError::OutOfMemory`] instead of growing.
/// A `Bump` can be moved to another thread (`Send`) but not shared between threads (`!Sync`).
///
/// Destructors are never run, which is why only `T: Copy` is accepted.
pub struct Bump {
    block: Block,
    offset: Cell<usize>,
    peak: Cell<usize>,
}

impl Bump {
    /// A bump allocator with room for `capacity` bytes.
    ///
    /// # Errors
    ///
    /// * [`MemoryError::SizeOverflow`] when `capacity` is larger than `isize::MAX`.
    /// * [`MemoryError::AllocationFailed`] when the system allocator cannot provide the block.
    pub fn with_capacity(capacity: usize) -> MemoryResult<Self> {
        Ok(Self {
            block: Block::new(capacity)?,
            offset: Cell::new(0),
            peak: Cell::new(0),
        })
    }

    /// Like [`Bump::with_capacity`], for a capacity given as a [`ByteSize`].
    ///
    /// # Errors
    ///
    /// As [`Bump::with_capacity`]; also [`MemoryError::SizeOverflow`] when `size` does not fit in `usize`.
    pub fn with_byte_size(size: ByteSize) -> MemoryResult<Self> {
        let capacity = size.as_usize().ok_or(MemoryError::SizeOverflow)?;
        Self::with_capacity(capacity)
    }

    /// The total size of the block in bytes.
    pub fn capacity(&self) -> usize {
        self.block.layout.size()
    }

    /// The bytes used so far, including alignment padding.
    pub fn used(&self) -> usize {
        self.offset.get()
    }

    /// The bytes still free (before the padding a request may need).
    pub fn remaining(&self) -> usize {
        self.capacity() - self.used()
    }

    /// The highest value [`Bump::used`] has reached since the allocator was created; it survives
    /// [`reset`](Bump::reset) and [`rewind`](Bump::rewind). Use it to size the capacity.
    pub fn peak(&self) -> usize {
        self.peak.get()
    }

    /// The current position, to [`rewind`](Bump::rewind) to later.
    pub fn marker(&self) -> Marker {
        Marker(self.offset.get())
    }

    /// Frees everything allocated after `marker`.
    ///
    /// This needs `&mut self`, so every reference handed out before (not only those after the marker) must be dead
    /// by now; the compiler enforces it.
    ///
    /// # Errors
    ///
    /// [`MemoryError::InvalidMarker`] when `marker` lies beyond the current position, for example because the
    /// allocator was reset after the marker was taken.
    pub fn rewind(&mut self, marker: Marker) -> MemoryResult<()> {
        let offset = self.offset.get_mut();
        if marker.0 > *offset {
            return Err(MemoryError::InvalidMarker);
        }
        *offset = marker.0;
        Ok(())
    }

    /// Frees everything. The block is kept and reused; [`Bump::peak`] is not reset.
    pub fn reset(&mut self) {
        *self.offset.get_mut() = 0;
    }

    /// Reserves `layout.size()` bytes aligned to `layout.align()` and returns the address of the first one.
    ///
    /// The memory is uninitialised and has not been handed out since the last reset. For a zero-sized layout the
    /// result is a dangling, aligned pointer and nothing is reserved.
    fn alloc_layout(&self, layout: Layout) -> MemoryResult<NonNull<u8>> {
        if layout.size() == 0 {
            let dangling = std::ptr::without_provenance_mut::<u8>(layout.align());
            return Ok(NonNull::new(dangling).unwrap_or(NonNull::dangling()));
        }

        let used = self.offset.get();
        let base = self.block.ptr.as_ptr().addr();

        // Align the address, not the offset: the block itself is only aligned to `BLOCK_ALIGN`.
        let start = base.checked_add(used).ok_or(MemoryError::SizeOverflow)?;
        let aligned = Alignment::of_layout(layout).align_up(start)?;
        let begin = aligned - base; // `aligned >= start >= base`
        let end = begin
            .checked_add(layout.size())
            .ok_or(MemoryError::SizeOverflow)?;

        if end > self.capacity() {
            return Err(MemoryError::OutOfMemory {
                requested: layout.size(),
                available: self.remaining(),
            });
        }

        self.offset.set(end);
        if end > self.peak.get() {
            self.peak.set(end);
        }

        // SAFETY: `begin < end <= capacity` and `layout.size() > 0`, so the capacity is positive: the block is a
        // real allocation of `capacity` bytes and `begin` is an offset inside it.
        Ok(unsafe { self.block.ptr.add(begin) })
    }

    /// Stores `value` and returns a reference to it.
    ///
    /// # Errors
    ///
    /// [`MemoryError::OutOfMemory`] when the value does not fit.
    // A `&self` -> `&mut T` signature is what makes an arena usable; it is sound because every call hands out a
    // region that no other call has handed out (see the module docs).
    #[allow(clippy::mut_from_ref)]
    pub fn alloc<T: Copy>(&self, value: T) -> MemoryResult<&mut T> {
        let ptr = self.alloc_layout(Layout::new::<T>())?.cast::<T>();
        // SAFETY: `ptr` is aligned for `T` and points to `size_of::<T>()` bytes inside the block that no other
        // reference covers (or is a dangling, aligned pointer for a zero-sized `T`, which is valid to write and
        // read). It is written before the reference exists, and the reference's lifetime is the borrow of `self`.
        unsafe {
            ptr.write(value);
            Ok(&mut *ptr.as_ptr())
        }
    }

    /// Copies `src` into the allocator and returns the copy.
    ///
    /// # Errors
    ///
    /// [`MemoryError::OutOfMemory`] when the copy does not fit.
    #[allow(clippy::mut_from_ref)] // see `alloc`
    pub fn alloc_slice_copy<T: Copy>(&self, src: &[T]) -> MemoryResult<&mut [T]> {
        let ptr = self.alloc_layout(Layout::for_value(src))?.cast::<T>();
        // SAFETY: the destination holds `src.len()` aligned, uninitialised slots of `T` that nothing else covers,
        // and `src` cannot overlap it because that memory was never handed out before. After the copy all slots
        // are initialised, so the slice is valid.
        unsafe {
            core::ptr::copy_nonoverlapping(src.as_ptr(), ptr.as_ptr(), src.len());
            Ok(core::slice::from_raw_parts_mut(ptr.as_ptr(), src.len()))
        }
    }

    /// A slice of `len` copies of `value`.
    ///
    /// # Errors
    ///
    /// * [`MemoryError::SizeOverflow`] when `len` elements would be larger than `isize::MAX` bytes.
    /// * [`MemoryError::OutOfMemory`] when the slice does not fit.
    #[allow(clippy::mut_from_ref)] // see `alloc`
    pub fn alloc_slice_fill<T: Copy>(&self, len: usize, value: T) -> MemoryResult<&mut [T]> {
        self.alloc_slice_fill_with(len, |_| value)
    }

    /// A slice of `len` elements, element `i` being `init(i)`; `init` is called in index order.
    ///
    /// For a zero-sized `T` nothing is written and `init` is not called.
    ///
    /// # Errors
    ///
    /// * [`MemoryError::SizeOverflow`] when `len` elements would be larger than `isize::MAX` bytes.
    /// * [`MemoryError::OutOfMemory`] when the slice does not fit.
    #[allow(clippy::mut_from_ref)] // see `alloc`
    pub fn alloc_slice_fill_with<T: Copy>(
        &self,
        len: usize,
        mut init: impl FnMut(usize) -> T,
    ) -> MemoryResult<&mut [T]> {
        let layout = array_layout(Layout::new::<T>(), len)?;
        let ptr = self.alloc_layout(layout)?.cast::<T>();

        if size_of::<T>() != 0 {
            for index in 0..len {
                let value = init(index);
                // SAFETY: `index < len`, and the region holds `len` aligned slots of `T` that nothing else covers.
                // `T: Copy`, so overwriting uninitialised memory drops nothing.
                unsafe { ptr.add(index).write(value) };
            }
        }

        // SAFETY: all `len` slots are initialised (the loop ran to the end; a panic in `init` unwinds before this
        // point and the slice never exists). For a zero-sized `T` there is nothing to initialise and a dangling,
        // aligned pointer is valid for any length.
        Ok(unsafe { core::slice::from_raw_parts_mut(ptr.as_ptr(), len) })
    }
}

impl fmt::Debug for Bump {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Bump")
            .field("capacity", &self.capacity())
            .field("used", &self.used())
            .field("peak", &self.peak())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[repr(align(32))]
    #[derive(Clone, Copy)]
    struct Aligned32([u8; 32]);

    fn is_aligned<T: ?Sized>(reference: &T, align: usize) -> bool {
        Alignment::new(align)
            .unwrap()
            .is_ptr_aligned(std::ptr::from_ref(reference))
    }

    fn oom(requested: usize, available: usize) -> MemoryError {
        MemoryError::OutOfMemory {
            requested,
            available,
        }
    }

    #[test]
    fn a_new_allocator_is_empty() {
        let bump = Bump::with_capacity(256).unwrap();
        assert_eq!(bump.capacity(), 256);
        assert_eq!(bump.used(), 0);
        assert_eq!(bump.remaining(), 256);
        assert_eq!(bump.peak(), 0);
    }

    #[test]
    fn can_be_sized_with_a_byte_size() {
        let bump = Bump::with_byte_size(ByteSize::from_kib(4)).unwrap();
        assert_eq!(bump.capacity(), 4096);
    }

    #[test]
    fn rejects_capacities_above_isize_max() {
        assert_eq!(
            Bump::with_capacity(usize::MAX).unwrap_err(),
            MemoryError::SizeOverflow
        );
    }

    #[test]
    fn stores_and_returns_values() {
        let bump = Bump::with_capacity(256).unwrap();
        let a = bump.alloc(7u32).unwrap();
        let b = bump.alloc(-3i64).unwrap();
        let c = bump.alloc([1u8, 2, 3]).unwrap();
        *a += 1;
        assert_eq!((*a, *b, *c), (8, -3, [1, 2, 3]));
    }

    #[test]
    fn values_do_not_overlap() {
        let bump = Bump::with_capacity(4 * 100).unwrap();
        let values: Vec<&mut u32> = (0..100u32).map(|i| bump.alloc(i).unwrap()).collect();
        for (index, value) in values.iter().enumerate() {
            assert_eq!(**value, u32::try_from(index).unwrap());
        }
        assert_eq!(bump.used(), 400);
    }

    #[test]
    fn respects_the_alignment_of_every_request() {
        let bump = Bump::with_capacity(512).unwrap();
        for _ in 0..4 {
            let _ = bump.alloc(1u8).unwrap();
            let wide = bump.alloc(2u64).unwrap();
            assert!(is_aligned(wide, std::mem::align_of::<u64>()));
            let over = bump.alloc(Aligned32([7; 32])).unwrap();
            assert!(is_aligned(over, 32));
            assert_eq!(over.0[31], 7);
        }
    }

    #[test]
    fn reports_exhaustion_with_the_numbers() {
        let bump = Bump::with_capacity(16).unwrap();
        let _ = bump.alloc([0u8; 16]).unwrap();
        assert_eq!(bump.used(), 16);
        assert_eq!(bump.alloc(1u8).unwrap_err(), oom(1, 0));
        // A failed request changes nothing.
        assert_eq!(bump.used(), 16);
    }

    #[test]
    fn too_large_requests_fail_and_leave_the_allocator_usable() {
        let bump = Bump::with_capacity(16).unwrap();
        assert_eq!(bump.alloc([0u8; 17]).unwrap_err(), oom(17, 16));
        assert_eq!(bump.used(), 0);
        assert!(bump.alloc([0u8; 16]).is_ok());
    }

    #[test]
    fn reset_reuses_the_block_and_keeps_the_peak() {
        let mut bump = Bump::with_capacity(8).unwrap();
        let _ = bump.alloc(1u64).unwrap();
        assert_eq!(bump.used(), 8);
        bump.reset();
        assert_eq!(bump.used(), 0);
        assert_eq!(bump.peak(), 8);
        assert_eq!(*bump.alloc(2u64).unwrap(), 2);
    }

    #[test]
    fn rewind_frees_back_to_a_marker() {
        let mut bump = Bump::with_capacity(64).unwrap();
        let _ = bump.alloc(1u32).unwrap();
        let marker = bump.marker();
        let _ = bump.alloc([0u8; 24]).unwrap();
        assert!(bump.used() > 4);
        bump.rewind(marker).unwrap();
        assert_eq!(bump.marker(), marker);
        assert_eq!(bump.used(), 4);
        assert!(bump.peak() >= 28);
    }

    #[test]
    fn rewinding_to_a_marker_from_the_future_is_an_error() {
        let mut bump = Bump::with_capacity(64).unwrap();
        let _ = bump.alloc([0u8; 8]).unwrap();
        let marker = bump.marker();
        bump.reset();
        assert_eq!(bump.rewind(marker), Err(MemoryError::InvalidMarker));
        assert_eq!(bump.used(), 0);
    }

    #[test]
    fn copies_slices() {
        let bump = Bump::with_capacity(64).unwrap();
        let copy = bump.alloc_slice_copy(&[1u16, 2, 3, 4]).unwrap();
        copy[0] = 9;
        assert_eq!(copy, &[9, 2, 3, 4]);
    }

    #[test]
    fn fills_slices() {
        let bump = Bump::with_capacity(128).unwrap();
        assert_eq!(bump.alloc_slice_fill(4, 5u8).unwrap(), &[5, 5, 5, 5]);

        let mut calls = Vec::new();
        let squares = bump
            .alloc_slice_fill_with(4, |index| {
                calls.push(index);
                index * index
            })
            .unwrap();
        assert_eq!(squares, &[0, 1, 4, 9]);
        assert_eq!(calls, [0, 1, 2, 3]);
    }

    #[test]
    fn empty_slices_take_no_room() {
        let bump = Bump::with_capacity(8).unwrap();
        let empty = bump.alloc_slice_fill(0, 1u64).unwrap();
        assert!(empty.is_empty());
        assert!(is_aligned(empty, std::mem::align_of::<u64>()));
        assert_eq!(bump.used(), 0);
    }

    #[test]
    fn huge_slices_are_errors_not_panics() {
        let bump = Bump::with_capacity(64).unwrap();
        assert_eq!(
            bump.alloc_slice_fill(usize::MAX, 0u64).unwrap_err(),
            MemoryError::SizeOverflow
        );
        assert_eq!(bump.alloc_slice_fill(100, 0u64).unwrap_err(), oom(800, 64));
        assert_eq!(bump.used(), 0);
    }

    #[test]
    fn zero_sized_values_need_no_room() {
        let bump = Bump::with_capacity(0).unwrap();
        assert_eq!(bump.capacity(), 0);
        let _ = bump.alloc(()).unwrap();
        let units = bump.alloc_slice_fill(usize::MAX, ()).unwrap();
        assert_eq!(units.len(), usize::MAX);
        assert_eq!(bump.used(), 0);
        assert_eq!(bump.alloc(1u8).unwrap_err(), oom(1, 0));
    }

    #[test]
    fn can_move_to_another_thread() {
        fn assert_send<T: Send>() {}
        assert_send::<Bump>();

        let bump = Bump::with_capacity(32).unwrap();
        let used = std::thread::spawn(move || {
            let _ = bump.alloc(1u64).unwrap();
            bump.used()
        })
        .join()
        .unwrap();
        assert_eq!(used, 8);
    }

    #[test]
    fn debug_output_shows_the_counters() {
        let bump = Bump::with_capacity(32).unwrap();
        let _ = bump.alloc(1u64).unwrap();
        assert_eq!(
            format!("{bump:?}"),
            "Bump { capacity: 32, used: 8, peak: 8 }"
        );
    }
}
