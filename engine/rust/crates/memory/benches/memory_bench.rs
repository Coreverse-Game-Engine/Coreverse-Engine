//! Criterion benchmarks for the `memory` crate: the arenas against the heap, the pool, the counters and the
//! overhead of the `TrackingAllocator`.
//!
//! Run with `cargo bench -p memory`. Every input goes through `black_box` (or the result is returned to criterion)
//! so the optimizer cannot fold the work away. The numbers depend on the machine and on the allocator of the
//! platform; read them as ratios between the benchmarks of one run, not as absolute figures.
#![allow(missing_docs)]
#![allow(
    unsafe_code,
    reason = "the allocator benchmarks call `GlobalAlloc` methods directly, which are unsafe"
)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use memory::{Bump, FrameArena, MemoryTag, MemoryTracker, Pool, TrackingAllocator};

/// How many values one iteration of the arena and pool benchmarks handles.
const COUNT: usize = 1000;

fn arenas(c: &mut Criterion) {
    let mut bump = Bump::with_capacity(64 * 1024).unwrap();

    c.bench_function("bump/alloc_usize_x1000", |bench| {
        bench.iter(|| {
            bump.reset();
            let mut sum = 0usize;
            for value in 0..COUNT {
                let slot = bump.alloc(black_box(value)).unwrap();
                sum = sum.wrapping_add(*slot);
            }
            sum
        })
    });

    // The baseline: the same values on the heap, freed at the end of the iteration.
    c.bench_function("heap/box_usize_x1000", |bench| {
        bench.iter(|| {
            let mut boxes = Vec::with_capacity(COUNT);
            for value in 0..COUNT {
                boxes.push(Box::new(black_box(value)));
            }
            let mut sum = 0usize;
            for boxed in &boxes {
                sum = sum.wrapping_add(**boxed);
            }
            sum
        })
    });

    c.bench_function("bump/alloc_slice_fill_256_f32", |bench| {
        bench.iter(|| {
            bump.reset();
            let slice = bump.alloc_slice_fill(256, black_box(1.0f32)).unwrap();
            slice[255]
        })
    });

    c.bench_function("heap/vec_256_f32", |bench| {
        bench.iter(|| {
            let values = vec![black_box(1.0f32); 256];
            values[255]
        })
    });

    let mut frames = FrameArena::with_capacity(64 * 1024).unwrap();
    c.bench_function("frame_arena/frame_of_100_allocs", |bench| {
        bench.iter(|| {
            frames.begin_frame();
            let mut sum = 0usize;
            for value in 0..100usize {
                let slot = frames.alloc(black_box(value)).unwrap();
                sum = sum.wrapping_add(*slot);
            }
            sum
        })
    });
}

fn pools(c: &mut Criterion) {
    let mut pool: Pool<usize> = Pool::with_capacity(COUNT).unwrap();
    let mut handles = Vec::with_capacity(COUNT);

    c.bench_function("pool/insert_then_remove_x1000", |bench| {
        bench.iter(|| {
            for value in 0..COUNT {
                handles.push(pool.insert(black_box(value)).unwrap());
            }
            let mut sum = 0usize;
            for handle in handles.drain(..) {
                sum = sum.wrapping_add(pool.remove(handle).unwrap());
            }
            sum
        })
    });

    let mut full: Pool<usize> = Pool::with_capacity(COUNT).unwrap();
    let live: Vec<_> = (0..COUNT)
        .map(|value| full.insert(value).unwrap())
        .collect();

    c.bench_function("pool/get_x1000", |bench| {
        bench.iter(|| {
            let mut sum = 0usize;
            for handle in &live {
                let value = full.get(black_box(*handle)).unwrap();
                sum = sum.wrapping_add(*value);
            }
            sum
        })
    });
}

fn tracking(c: &mut Criterion) {
    let tracker = MemoryTracker::new();
    c.bench_function("tracker/record_alloc_and_dealloc", |bench| {
        bench.iter(|| {
            tracker.record_alloc(MemoryTag::Ecs, black_box(64));
            tracker.record_dealloc(MemoryTag::Ecs, black_box(64));
        })
    });

    // The cost of counting: the same allocation with and without the wrapper.
    let layout = Layout::from_size_align(64, 8).unwrap();

    c.bench_function("allocator/system_alloc_dealloc_64", |bench| {
        bench.iter(|| {
            // SAFETY: non-zero size; the block is freed at once with the same layout.
            unsafe {
                let ptr = black_box(System.alloc(layout));
                assert!(!ptr.is_null());
                System.dealloc(ptr, layout);
            }
        })
    });

    let tracked = TrackingAllocator::new(System);
    c.bench_function("allocator/tracking_alloc_dealloc_64", |bench| {
        bench.iter(|| {
            // SAFETY: as above.
            unsafe {
                let ptr = black_box(tracked.alloc(layout));
                assert!(!ptr.is_null());
                tracked.dealloc(ptr, layout);
            }
        })
    });
}

criterion_group!(benches, arenas, pools, tracking);
criterion_main!(benches);
