//! The scratch arenas of a pool show up in the global memory tracker under the tag they were configured with.
//!
//! This test lives in a binary of its own on purpose: the tracker is process-wide, and the numbers below would
//! be disturbed by any other test that allocates tagged memory at the same time.

use memory::{MemoryTag, global_tracker};
use threading::{PoolConfig, ThreadPool, WorkerCount};

fn live_bytes(tag: MemoryTag) -> u64 {
    global_tracker().stats(tag).current.bytes()
}

#[test]
fn the_arenas_are_registered_with_their_tag_and_released_with_the_pool() {
    let tag = MemoryTag::Tools;
    let before = live_bytes(tag);

    let pool = ThreadPool::new(
        PoolConfig::new()
            .with_workers(WorkerCount::Exact(3))
            .with_scratch(16 * 1024)
            .with_scratch_tag(tag),
    )
    .unwrap();
    assert_eq!(
        live_bytes(tag) - before,
        3 * 16 * 1024,
        "one arena per worker"
    );
    assert_eq!(pool.stats().scratch_capacity, 16 * 1024);

    drop(pool);
    assert_eq!(
        live_bytes(tag),
        before,
        "the workers freed their arenas when they left"
    );
}

#[test]
fn the_default_tag_is_general_and_a_pool_without_scratch_registers_nothing() {
    let general = live_bytes(MemoryTag::General);
    let plain = ThreadPool::new(PoolConfig::new().with_workers(WorkerCount::Exact(2))).unwrap();
    assert_eq!(live_bytes(MemoryTag::General), general);
    drop(plain);

    let tagged = ThreadPool::new(
        PoolConfig::new()
            .with_workers(WorkerCount::Exact(1))
            .with_scratch(4096),
    )
    .unwrap();
    assert_eq!(live_bytes(MemoryTag::General) - general, 4096);
    drop(tagged);
    assert_eq!(live_bytes(MemoryTag::General), general);
}
