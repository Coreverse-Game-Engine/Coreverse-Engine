//! A pool of values addressed by generation-checked handles.

use core::fmt;

use ccore::Handle;

use crate::error::{MemoryError, MemoryResult};

/// One place in the pool. `value` is `Some` while the slot is occupied.
///
/// A free slot is linked into the free list through `next_free`. A slot with no value that is not in the free list
/// is *retired*: its generation counter ran out and it is never used again.
struct Slot<T> {
    generation: u32,
    value: Option<T>,
    next_free: Option<u32>,
}

/// The error for a request that failed to reserve memory for `count` more slots.
fn allocation_failed<T>(count: usize) -> MemoryError {
    MemoryError::AllocationFailed {
        size: count.saturating_mul(size_of::<Slot<T>>()),
    }
}

/// A pool of `T` values that are addressed by [`Handle<T>`] instead of references.
///
/// Inserting a value returns a handle (a slot index plus a generation). Removing the value bumps the slot's
/// generation, so every older handle to it becomes *stale*: [`Pool::get`] and friends answer `None` for it, even
/// after the slot has been reused for another value. A stale or invented handle is therefore never undefined
/// behaviour and never reaches the wrong value; it is just an invalid handle. The pool is entirely safe code.
///
/// ```
/// use memory::Pool;
///
/// let mut textures: Pool<String> = Pool::new();
/// let grass = textures.insert("grass.png".to_string())?;
/// let rock = textures.insert("rock.png".to_string())?;
/// assert_eq!(textures.len(), 2);
/// assert_eq!(textures.get(rock).map(String::as_str), Some("rock.png"));
///
/// assert_eq!(textures.remove(grass).as_deref(), Some("grass.png"));
/// let sand = textures.insert("sand.png".to_string())?;
/// assert_eq!(sand.index(), grass.index()); // the slot was reused ...
/// assert!(textures.get(grass).is_none()); // ... but the old handle is stale
/// assert!(textures.get(sand).is_some());
/// # Ok::<(), memory::MemoryError>(())
/// ```
///
/// Things to know:
///
/// * **Handles do not know their pool.** A handle from one `Pool<T>` used on another `Pool<T>` may find an unrelated
///   value (or none). Keep a handle with the pool that issued it.
/// * **Slots are reused, last freed first.** The pool grows when no slot is free and never shrinks.
///   [`Pool::with_capacity`] reserves room up front so that inserts up to that many values do not allocate.
/// * **A slot is retired after 2^32 - 1 reuses.** Its generation counter is then exhausted and the slot is never used
///   again (see [`Pool::retired_slots`]) instead of letting very old handles look valid again.
/// * **At most 2^32 slots**, because an index is a `u32`.
pub struct Pool<T> {
    slots: Vec<Slot<T>>,
    free_head: Option<u32>,
    len: usize,
    retired: usize,
}

impl<T> Pool<T> {
    /// An empty pool. Nothing is allocated until the first insert.
    pub const fn new() -> Self {
        Self {
            slots: Vec::new(),
            free_head: None,
            len: 0,
            retired: 0,
        }
    }

    /// An empty pool with room for `capacity` values: that many inserts do not allocate.
    ///
    /// # Errors
    ///
    /// [`MemoryError::AllocationFailed`] when the memory cannot be reserved (or `capacity` slots would be larger than
    /// `isize::MAX` bytes).
    pub fn with_capacity(capacity: usize) -> MemoryResult<Self> {
        let mut pool = Self::new();
        pool.slots
            .try_reserve_exact(capacity)
            .map_err(|_| allocation_failed::<T>(capacity))?;
        Ok(pool)
    }

    /// The number of values in the pool.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether the pool holds no value.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The number of slots the pool can hold without allocating.
    pub fn capacity(&self) -> usize {
        self.slots.capacity()
    }

    /// The number of slots that were retired because their generation counter ran out. They are lost until the pool
    /// is dropped.
    pub fn retired_slots(&self) -> usize {
        self.retired
    }

    /// Stores `value` and returns its handle.
    ///
    /// # Errors
    ///
    /// * [`MemoryError::PoolFull`] when the pool already uses 2^32 slots.
    /// * [`MemoryError::AllocationFailed`] when growing the pool fails.
    ///
    /// On an error `value` is dropped.
    pub fn insert(&mut self, value: T) -> MemoryResult<Handle<T>> {
        let (index, generation) = if let Some(index) = self.free_head {
            let slot = &mut self.slots[index as usize];
            self.free_head = slot.next_free.take();
            slot.value = Some(value);
            (index, slot.generation)
        } else {
            let index = u32::try_from(self.slots.len()).map_err(|_| MemoryError::PoolFull)?;
            self.slots
                .try_reserve(1)
                .map_err(|_| allocation_failed::<T>(1))?;
            self.slots.push(Slot {
                generation: 0,
                value: Some(value),
                next_free: None,
            });
            (index, 0)
        };
        self.len += 1;
        Ok(Handle::new(index, generation))
    }

    /// The value `handle` refers to, or `None` when the handle is stale or was never issued by this pool.
    pub fn get(&self, handle: Handle<T>) -> Option<&T> {
        let slot = self.slots.get(handle.index() as usize)?;
        if slot.generation == handle.generation() {
            slot.value.as_ref()
        } else {
            None
        }
    }

    /// Like [`Pool::get`], for a mutable reference.
    pub fn get_mut(&mut self, handle: Handle<T>) -> Option<&mut T> {
        let slot = self.slots.get_mut(handle.index() as usize)?;
        if slot.generation == handle.generation() {
            slot.value.as_mut()
        } else {
            None
        }
    }

    /// Whether `handle` refers to a value in the pool.
    pub fn contains(&self, handle: Handle<T>) -> bool {
        self.get(handle).is_some()
    }

    /// Takes the value `handle` refers to out of the pool, or `None` when the handle is stale or invalid. Every
    /// handle to the value is stale afterwards.
    pub fn remove(&mut self, handle: Handle<T>) -> Option<T> {
        let index = handle.index() as usize;
        if self.slots.get(index)?.generation != handle.generation() {
            return None;
        }
        self.free_slot(index)
    }

    /// Removes every value (dropping them) and makes all handles stale. The slots are kept for reuse.
    pub fn clear(&mut self) {
        for index in 0..self.slots.len() {
            drop(self.free_slot(index));
        }
    }

    /// The occupied slots in index order, with the handle of each.
    pub fn iter(&self) -> impl Iterator<Item = (Handle<T>, &T)> {
        let slots = self.slots.iter();
        slots.enumerate().filter_map(|(index, slot)| {
            let value = slot.value.as_ref()?;
            let index = u32::try_from(index).ok()?;
            Some((Handle::new(index, slot.generation), value))
        })
    }

    /// Like [`Pool::iter`], with mutable references.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (Handle<T>, &mut T)> {
        let slots = self.slots.iter_mut();
        slots.enumerate().filter_map(|(index, slot)| {
            let generation = slot.generation;
            let value = slot.value.as_mut()?;
            let index = u32::try_from(index).ok()?;
            Some((Handle::new(index, generation), value))
        })
    }

    /// Empties the slot at `index` and bumps its generation. `None` when the slot does not exist or is empty.
    fn free_slot(&mut self, index: usize) -> Option<T> {
        let slot = self.slots.get_mut(index)?;
        let value = slot.value.take()?;
        self.len -= 1;

        match slot.generation.checked_add(1) {
            Some(next) => {
                slot.generation = next;
                slot.next_free = self.free_head;
                self.free_head = u32::try_from(index).ok();
            }
            // The counter is exhausted: leave the slot empty and out of the free list for good.
            None => self.retired += 1,
        }
        Some(value)
    }
}

impl<T> Default for Pool<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> fmt::Debug for Pool<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pool")
            .field("len", &self.len)
            .field("capacity", &self.capacity())
            .field("retired_slots", &self.retired)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use super::*;

    /// Counts its own drops.
    struct DropCounter(Rc<Cell<u32>>);

    impl Drop for DropCounter {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
        }
    }

    #[test]
    fn a_new_pool_is_empty() {
        let pool: Pool<u32> = Pool::new();
        assert_eq!(pool.len(), 0);
        assert!(pool.is_empty());
        assert_eq!(pool.retired_slots(), 0);
        assert_eq!(pool.iter().count(), 0);
        assert_eq!(Pool::<u32>::default().len(), 0);
    }

    #[test]
    fn stores_and_finds_values() {
        let mut pool = Pool::new();
        let a = pool.insert("a".to_string()).unwrap();
        let b = pool.insert("b".to_string()).unwrap();
        assert_ne!(a, b);
        assert_eq!(pool.len(), 2);
        assert_eq!(pool.get(a).map(String::as_str), Some("a"));
        assert_eq!(pool.get(b).map(String::as_str), Some("b"));
        assert!(pool.contains(a));

        pool.get_mut(a).unwrap().push('!');
        assert_eq!(pool.get(a).map(String::as_str), Some("a!"));
    }

    #[test]
    fn removing_returns_the_value_and_makes_the_handle_stale() {
        let mut pool = Pool::new();
        let handle = pool.insert(7u32).unwrap();
        assert_eq!(pool.remove(handle), Some(7));
        assert!(pool.is_empty());

        assert_eq!(pool.get(handle), None);
        assert_eq!(pool.get_mut(handle), None);
        assert!(!pool.contains(handle));
        assert_eq!(pool.remove(handle), None);
        assert_eq!(pool.len(), 0);
    }

    #[test]
    fn a_reused_slot_gets_a_new_generation() {
        let mut pool = Pool::new();
        let old = pool.insert(1u32).unwrap();
        assert_eq!((old.index(), old.generation()), (0, 0));
        pool.remove(old).unwrap();

        let new = pool.insert(2u32).unwrap();
        assert_eq!((new.index(), new.generation()), (0, 1));
        assert_eq!(pool.get(old), None);
        assert_eq!(pool.get(new), Some(&2));
        // The stale handle cannot remove the new occupant.
        assert_eq!(pool.remove(old), None);
        assert_eq!(pool.get(new), Some(&2));
    }

    #[test]
    fn the_last_freed_slot_is_reused_first() {
        let mut pool = Pool::new();
        let a = pool.insert('a').unwrap();
        let _b = pool.insert('b').unwrap();
        let c = pool.insert('c').unwrap();
        pool.remove(a).unwrap();
        pool.remove(c).unwrap();

        assert_eq!(pool.insert('d').unwrap().index(), 2);
        assert_eq!(pool.insert('e').unwrap().index(), 0);
        assert_eq!(pool.insert('f').unwrap().index(), 3);
        assert_eq!(pool.len(), 4);
    }

    #[test]
    fn invented_handles_find_nothing() {
        let mut pool = Pool::new();
        let _ = pool.insert(1u32).unwrap();
        assert_eq!(pool.get(Handle::new(999, 0)), None);
        assert_eq!(pool.get(Handle::new(0, 5)), None);
        assert_eq!(pool.get(Handle::new(u32::MAX, u32::MAX)), None);
        assert_eq!(pool.remove(Handle::new(999, 0)), None);
        assert_eq!(pool.len(), 1);
    }

    #[test]
    fn reserved_capacity_means_no_allocation() {
        let mut pool = Pool::<u64>::with_capacity(16).unwrap();
        let capacity = pool.capacity();
        assert!(capacity >= 16);
        for value in 0..16 {
            let _ = pool.insert(value).unwrap();
        }
        assert_eq!(pool.capacity(), capacity);
    }

    #[test]
    fn absurd_capacities_are_errors_not_aborts() {
        let result = Pool::<u64>::with_capacity(usize::MAX);
        assert!(matches!(result, Err(MemoryError::AllocationFailed { .. })));
    }

    #[test]
    fn a_slot_is_retired_when_its_generation_runs_out() {
        let mut pool = Pool::new();
        let first = pool.insert(1u32).unwrap();
        pool.slots[0].generation = u32::MAX; // as if it had been reused 2^32 - 1 times
        let last = Handle::new(first.index(), u32::MAX);

        assert_eq!(pool.remove(last), Some(1));
        assert_eq!(pool.retired_slots(), 1);
        assert!(pool.is_empty());

        // The retired slot is skipped: the next value goes to a new slot.
        let second = pool.insert(2u32).unwrap();
        assert_eq!(second.index(), 1);
        assert_eq!(pool.get(last), None);
        assert_eq!(pool.remove(last), None);
        assert_eq!(pool.get(second), Some(&2));
    }

    #[test]
    fn iterates_over_the_occupied_slots_with_their_handles() {
        let mut pool = Pool::new();
        let handles: Vec<_> = (10..15u32)
            .map(|value| pool.insert(value).unwrap())
            .collect();
        pool.remove(handles[1]).unwrap();
        pool.remove(handles[3]).unwrap();

        let seen: Vec<(u32, u32)> = pool
            .iter()
            .map(|(handle, value)| (handle.index(), *value))
            .collect();
        assert_eq!(seen, [(0, 10), (2, 12), (4, 14)]);
        for (handle, value) in pool.iter() {
            assert_eq!(pool.get(handle), Some(value));
        }
    }

    #[test]
    fn iter_mut_changes_the_values_in_place() {
        let mut pool = Pool::new();
        let a = pool.insert(1u32).unwrap();
        let b = pool.insert(2u32).unwrap();
        for (_, value) in pool.iter_mut() {
            *value *= 10;
        }
        assert_eq!((pool.get(a), pool.get(b)), (Some(&10), Some(&20)));
    }

    #[test]
    fn clear_drops_everything_and_makes_all_handles_stale() {
        let drops = Rc::new(Cell::new(0));
        let mut pool = Pool::new();
        let handles: Vec<_> = (0..3)
            .map(|_| pool.insert(DropCounter(Rc::clone(&drops))).unwrap())
            .collect();

        pool.clear();
        assert_eq!(drops.get(), 3);
        assert!(pool.is_empty());
        assert!(handles.iter().all(|handle| !pool.contains(*handle)));

        // The slots are reused, with new generations.
        let again = pool.insert(DropCounter(Rc::clone(&drops))).unwrap();
        assert!(again.generation() >= 1);
        assert_eq!(pool.len(), 1);
    }

    #[test]
    fn values_are_dropped_exactly_once() {
        let drops = Rc::new(Cell::new(0));
        let mut pool = Pool::new();
        let kept = pool.insert(DropCounter(Rc::clone(&drops))).unwrap();
        let removed = pool.insert(DropCounter(Rc::clone(&drops))).unwrap();

        let value = pool.remove(removed).unwrap();
        assert_eq!(drops.get(), 0); // ownership moved to the caller
        drop(value);
        assert_eq!(drops.get(), 1);

        assert!(pool.contains(kept));
        drop(pool);
        assert_eq!(drops.get(), 2);
    }

    #[test]
    fn debug_output_shows_the_counters() {
        let mut pool = Pool::with_capacity(4).unwrap();
        let _ = pool.insert(1u8).unwrap();
        let text = format!("{pool:?}");
        assert!(text.starts_with("Pool { len: 1, capacity: "));
        assert!(text.ends_with(", retired_slots: 0 }"));
    }

    #[test]
    fn works_for_zero_sized_values() {
        let mut pool = Pool::new();
        let handle = pool.insert(()).unwrap();
        assert_eq!(pool.get(handle), Some(&()));
        assert_eq!(pool.remove(handle), Some(()));
    }
}
