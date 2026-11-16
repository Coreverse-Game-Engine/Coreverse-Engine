//! Memory FFI surface: the per-tag counters, budgets and report of the `memory`
//! crate's global [`MemoryTracker`](memory_crate::MemoryTracker), so that C++
//! can register the memory it allocates and read the numbers.
//!
//! The tracker only knows what is registered with it. Rust code registers
//! through the tagged arenas of the `memory` crate; C++ registers with
//! [`ffi_memory_record_alloc`] and [`ffi_memory_record_dealloc`] (the
//! `cv_ffi::memory` wrappers do it with RAII types and a `std::pmr` resource),
//! under one of the `FFI_MEMORY_TAG_*` categories. Both sides then share one
//! report.
//!
//! What crosses the boundary:
//!
//! * **Counters.** [`ffi_memory_tag_stats`] and [`ffi_memory_total_stats`] fill
//!   a [`MemoryStats`]: bytes in use, the peak, the total ever allocated and the
//!   number of calls.
//! * **Budgets.** [`ffi_memory_set_budget`], [`ffi_memory_clear_budget`],
//!   [`ffi_memory_budget`] and [`ffi_memory_is_over_budget`]. A budget is
//!   advisory: nothing is refused when a tag goes over it.
//! * **The report.** [`ffi_memory_report_string`] returns it as text, and
//!   [`ffi_memory_report_diagnostics`] turns the tags that still hold memory
//!   (leaks) and the tags over their budget into log diagnostics.
//!
//! What does not cross: the `TrackingAllocator` of the `memory` crate. It is a
//! global allocator, and the choice of the global allocator belongs to the
//! executable, not to this library; an executable that installs it reads its
//! numbers from Rust.
//!
//! ## Tags as `u8`
//! Every tag crosses as a `u8`, one of the `FFI_MEMORY_TAG_*` constants (the
//! same convention as the enums of the `platform` module). A byte that is not a
//! known tag is rejected: the call returns `false` (or null) and has no other
//! effect, so the `memory` crate can add tags without breaking an older caller.
//!
//! ## Errors
//! There is no last-error slot: the only way these calls can fail is an
//! unknown tag byte (or a panic caught at the boundary), and both are reported
//! by the return value alone.
//!
//! (The dependency is imported as `memory_crate` in `Cargo.toml` for the same
//! reason `vfs` is imported as `vfs_crate`: this module is itself named
//! `memory`.)
//!
//! ## Leak and budget diagnostics
//! [`ffi_memory_report_diagnostics`] emits through a [`Logger`] with the
//! producer `CV-MEMORY` ("Coreverse Memory"), registered at runtime on the
//! first call. A tag that still has bytes in use is `CV-MEMORY-LEAK-<n>`, a tag
//! over its budget is `CV-MEMORY-BUDGET-<n>`, where `<n>` is the tag number
//! plus one (`1` for `FFI_MEMORY_TAG_GENERAL`, `10` for
//! `FFI_MEMORY_TAG_TOOLS`). Both are warnings. Call it when the program shuts
//! down or a level ends: only then is "still in use" a leak.

use std::ffi::{CStr, CString};
use std::os::raw::c_char;

use memory_crate::{ByteSize, MemoryStats as CoreStats, MemoryTag, global_tracker};

use crate::guard;
use crate::logger::Logger;

/// Tag: memory that belongs to no particular system (the default).
pub const FFI_MEMORY_TAG_GENERAL: u8 = 0;
/// Tag: the renderer.
pub const FFI_MEMORY_TAG_RENDERING: u8 = 1;
/// Tag: assets and resources.
pub const FFI_MEMORY_TAG_ASSETS: u8 = 2;
/// Tag: the entity-component system.
pub const FFI_MEMORY_TAG_ECS: u8 = 3;
/// Tag: physics.
pub const FFI_MEMORY_TAG_PHYSICS: u8 = 4;
/// Tag: audio.
pub const FFI_MEMORY_TAG_AUDIO: u8 = 5;
/// Tag: scripting.
pub const FFI_MEMORY_TAG_SCRIPTING: u8 = 6;
/// Tag: user interface.
pub const FFI_MEMORY_TAG_UI: u8 = 7;
/// Tag: networking.
pub const FFI_MEMORY_TAG_NETWORKING: u8 = 8;
/// Tag: editor and tools.
pub const FFI_MEMORY_TAG_TOOLS: u8 = 9;
/// The number of tags: valid tag bytes are `0` up to this value, exclusive.
pub const FFI_MEMORY_TAG_COUNT: u8 = 10;
/// A tag this header does not know yet (a tag added to the `memory` crate
/// later). Never a valid argument.
pub const FFI_MEMORY_TAG_UNKNOWN: u8 = 255;

/// The producer code of the diagnostics of [`ffi_memory_report_diagnostics`].
const PRODUCER_CODE: &str = "CV-MEMORY";
/// The name shown for that producer.
const PRODUCER_NAME: &str = "Coreverse Memory";
/// The organization of that producer.
const PRODUCER_ORG: &str = "Coreverse";

/// Allocation counters in a C layout, filled by [`ffi_memory_tag_stats`] and
/// [`ffi_memory_total_stats`].
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MemoryStats {
    /// The bytes in use now.
    pub current_bytes: u64,
    /// The highest value `current_bytes` has reached.
    pub peak_bytes: u64,
    /// The sum of all bytes ever registered; it only grows.
    pub total_allocated_bytes: u64,
    /// The number of registered allocations.
    pub allocations: u64,
    /// The number of registered deallocations.
    pub deallocations: u64,
    /// The number of reallocations (always `0` for memory registered through
    /// this interface, which has no resize call).
    pub reallocations: u64,
}

impl From<CoreStats> for MemoryStats {
    fn from(stats: CoreStats) -> Self {
        Self {
            current_bytes: stats.current.bytes(),
            peak_bytes: stats.peak.bytes(),
            total_allocated_bytes: stats.total_allocated.bytes(),
            allocations: stats.allocations,
            deallocations: stats.deallocations,
            reallocations: stats.reallocations,
        }
    }
}

/// The tag a `FFI_MEMORY_TAG_*` byte stands for. Written out instead of
/// indexing `MemoryTag::ALL`, so that reordering the tags of the `memory` crate
/// cannot silently change this ABI (a unit test checks the two agree).
const fn tag_from_code(code: u8) -> Option<MemoryTag> {
    match code {
        FFI_MEMORY_TAG_GENERAL => Some(MemoryTag::General),
        FFI_MEMORY_TAG_RENDERING => Some(MemoryTag::Rendering),
        FFI_MEMORY_TAG_ASSETS => Some(MemoryTag::Assets),
        FFI_MEMORY_TAG_ECS => Some(MemoryTag::Ecs),
        FFI_MEMORY_TAG_PHYSICS => Some(MemoryTag::Physics),
        FFI_MEMORY_TAG_AUDIO => Some(MemoryTag::Audio),
        FFI_MEMORY_TAG_SCRIPTING => Some(MemoryTag::Scripting),
        FFI_MEMORY_TAG_UI => Some(MemoryTag::Ui),
        FFI_MEMORY_TAG_NETWORKING => Some(MemoryTag::Networking),
        FFI_MEMORY_TAG_TOOLS => Some(MemoryTag::Tools),
        _ => None,
    }
}

const fn tag_code(tag: MemoryTag) -> u8 {
    match tag {
        MemoryTag::General => FFI_MEMORY_TAG_GENERAL,
        MemoryTag::Rendering => FFI_MEMORY_TAG_RENDERING,
        MemoryTag::Assets => FFI_MEMORY_TAG_ASSETS,
        MemoryTag::Ecs => FFI_MEMORY_TAG_ECS,
        MemoryTag::Physics => FFI_MEMORY_TAG_PHYSICS,
        MemoryTag::Audio => FFI_MEMORY_TAG_AUDIO,
        MemoryTag::Scripting => FFI_MEMORY_TAG_SCRIPTING,
        MemoryTag::Ui => FFI_MEMORY_TAG_UI,
        MemoryTag::Networking => FFI_MEMORY_TAG_NETWORKING,
        MemoryTag::Tools => FFI_MEMORY_TAG_TOOLS,
        // `MemoryTag` is `#[non_exhaustive]`.
        _ => FFI_MEMORY_TAG_UNKNOWN,
    }
}

/// The name of a tag as a static C string (the same text as
/// `MemoryTag::name`, a unit test checks it).
const fn tag_name(tag: MemoryTag) -> &'static CStr {
    match tag {
        MemoryTag::General => c"General",
        MemoryTag::Rendering => c"Rendering",
        MemoryTag::Assets => c"Assets",
        MemoryTag::Ecs => c"Ecs",
        MemoryTag::Physics => c"Physics",
        MemoryTag::Audio => c"Audio",
        MemoryTag::Scripting => c"Scripting",
        MemoryTag::Ui => c"Ui",
        MemoryTag::Networking => c"Networking",
        MemoryTag::Tools => c"Tools",
        _ => c"Unknown",
    }
}

/// A byte count as `usize`, saturating on a target where `usize` is narrower
/// than 64 bits.
fn saturating_usize(bytes: u64) -> usize {
    usize::try_from(bytes).unwrap_or(usize::MAX)
}

/// The name of a tag as a static, NUL-terminated string, or null when `tag` is
/// not one of the `FFI_MEMORY_TAG_*` constants. The string lives for the whole
/// program: do NOT free it.
#[unsafe(no_mangle)]
pub extern "C" fn ffi_memory_tag_name(tag: u8) -> *const c_char {
    guard(std::ptr::null(), || match tag_from_code(tag) {
        Some(tag) => tag_name(tag).as_ptr(),
        None => std::ptr::null(),
    })
}

/// Registers an allocation of `bytes` bytes under `tag` in the process-wide
/// tracker. Returns `false`, registering nothing, when `tag` is not one of the
/// `FFI_MEMORY_TAG_*` constants.
///
/// The tracker only counts: it never allocates or frees anything, so the caller
/// must pair every call with [`ffi_memory_record_dealloc`] when it frees the
/// memory, with the same tag.
#[unsafe(no_mangle)]
pub extern "C" fn ffi_memory_record_alloc(tag: u8, bytes: u64) -> bool {
    guard(false, || {
        let Some(tag) = tag_from_code(tag) else {
            return false;
        };
        global_tracker().record_alloc(tag, saturating_usize(bytes));
        true
    })
}

/// Registers that `bytes` bytes registered under `tag` were freed. Returns
/// `false`, registering nothing, when `tag` is not one of the
/// `FFI_MEMORY_TAG_*` constants. Freeing more than was registered stops the
/// counter at zero instead of wrapping.
#[unsafe(no_mangle)]
pub extern "C" fn ffi_memory_record_dealloc(tag: u8, bytes: u64) -> bool {
    guard(false, || {
        let Some(tag) = tag_from_code(tag) else {
            return false;
        };
        global_tracker().record_dealloc(tag, saturating_usize(bytes));
        true
    })
}

/// Copies the counters of one tag into `*out`. Returns `false` and leaves
/// `*out` untouched when `tag` is not one of the `FFI_MEMORY_TAG_*` constants.
///
/// # Safety
/// `out` must be a valid, non-null, writable pointer to a [`MemoryStats`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ffi_memory_tag_stats(tag: u8, out: *mut MemoryStats) -> bool {
    guard(false, || {
        let Some(tag) = tag_from_code(tag) else {
            return false;
        };
        let stats = global_tracker().stats(tag);
        // SAFETY: the caller guarantees `out` is valid, non-null and writable.
        unsafe { out.write(stats.into()) };
        true
    })
}

/// Copies the counters of all tags together into `*out`. Always returns `true`
/// (`false` only if a Rust panic was caught at the boundary).
///
/// # Safety
/// `out` must be a valid, non-null, writable pointer to a [`MemoryStats`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ffi_memory_total_stats(out: *mut MemoryStats) -> bool {
    guard(false, || {
        let stats = global_tracker().total();
        // SAFETY: the caller guarantees `out` is valid, non-null and writable.
        unsafe { out.write(stats.into()) };
        true
    })
}

/// Sets the budget of `tag` to `bytes`. A budget is advisory: it changes
/// [`ffi_memory_is_over_budget`] and the report, nothing is refused. Returns
/// `false` when `tag` is not one of the `FFI_MEMORY_TAG_*` constants.
#[unsafe(no_mangle)]
pub extern "C" fn ffi_memory_set_budget(tag: u8, bytes: u64) -> bool {
    guard(false, || {
        let Some(tag) = tag_from_code(tag) else {
            return false;
        };
        global_tracker().set_budget(tag, Some(ByteSize::from_bytes(bytes)));
        true
    })
}

/// Removes the budget of `tag`. Returns `false` when `tag` is not one of the
/// `FFI_MEMORY_TAG_*` constants (a tag without a budget is fine).
#[unsafe(no_mangle)]
pub extern "C" fn ffi_memory_clear_budget(tag: u8) -> bool {
    guard(false, || {
        let Some(tag) = tag_from_code(tag) else {
            return false;
        };
        global_tracker().set_budget(tag, None);
        true
    })
}

/// Writes the budget of `tag` into `*out` and returns `true`; returns `false`
/// and leaves `*out` untouched when the tag has no budget or is not one of the
/// `FFI_MEMORY_TAG_*` constants.
///
/// # Safety
/// `out` must be a valid, non-null, writable pointer to a `u64`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ffi_memory_budget(tag: u8, out: *mut u64) -> bool {
    guard(false, || {
        let Some(tag) = tag_from_code(tag) else {
            return false;
        };
        let Some(budget) = global_tracker().budget(tag) else {
            return false;
        };
        // SAFETY: the caller guarantees `out` is valid, non-null and writable.
        unsafe { out.write(budget.bytes()) };
        true
    })
}

/// Whether `tag` has a budget and uses more than it. `false` also when `tag` is
/// not one of the `FFI_MEMORY_TAG_*` constants.
#[unsafe(no_mangle)]
pub extern "C" fn ffi_memory_is_over_budget(tag: u8) -> bool {
    guard(false, || {
        tag_from_code(tag).is_some_and(|tag| global_tracker().is_over_budget(tag))
    })
}

/// The report of the process-wide tracker as text: a `total:` line and one line
/// per tag that was used or has a budget. The caller MUST free the result with
/// [`crate::ffi_free_string`]. Null only if a Rust panic was caught at the
/// boundary.
#[unsafe(no_mangle)]
pub extern "C" fn ffi_memory_report_string() -> *mut c_char {
    guard(std::ptr::null_mut(), || {
        let report = global_tracker().report();
        // The text never contains a NUL byte; replace one anyway rather than
        // lose the whole report.
        let text = report.to_string().replace('\0', "\u{FFFD}");
        CString::new(text).map_or(std::ptr::null_mut(), CString::into_raw)
    })
}

/// Emits one warning through `logger` for every tag that still has bytes in
/// use (`CV-MEMORY-LEAK-<n>`) and one for every tag over its budget
/// (`CV-MEMORY-BUDGET-<n>`); see the module docs for the codes. Returns the
/// number of diagnostics emitted: `0` when everything is freed and within
/// budget, and also when `logger` is null.
///
/// A diagnostic below the logger's minimum severity is dropped by the logger,
/// but still counted here.
///
/// # Safety
/// `logger` must be null (nothing is emitted) or a valid pointer previously
/// returned by [`crate::logger::ffi_logger_create`] and not yet destroyed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ffi_memory_report_diagnostics(logger: *mut Logger) -> u32 {
    guard(0, || {
        if logger.is_null() {
            return 0;
        }
        // SAFETY: the caller guarantees a non-null `logger` is a live Logger.
        let logger = unsafe { &*logger };
        emit_report(logger)
    })
}

/// Emits the leak and budget diagnostics of the global tracker through
/// `logger`; returns how many.
fn emit_report(logger: &Logger) -> u32 {
    // The only failure is "already registered", which is exactly what we want.
    let _ = log_core::producer::register_dynamic(PRODUCER_CODE, PRODUCER_NAME, PRODUCER_ORG);

    let report = global_tracker().report();
    let mut emitted = 0;

    for entry in report.leaks() {
        let tag = entry.tag;
        let current = entry.stats.current;
        let live = entry.stats.live_allocations();
        let message = format!("{tag}: {current} still in use ({live} live)");
        emit(logger, "LEAK", tag, message);
        emitted += 1;
    }

    for entry in report.over_budget() {
        if let Some(budget) = entry.budget {
            let tag = entry.tag;
            let current = entry.stats.current;
            let message = format!("{tag}: {current} in use, budget {budget}");
            emit(logger, "BUDGET", tag, message);
            emitted += 1;
        }
    }
    emitted
}

fn emit(logger: &Logger, category: &str, tag: MemoryTag, message: String) {
    let number = u32::from(tag_code(tag)) + 1;
    let diagnostic = log_core::DiagnosticBuilder::new(
        PRODUCER_CODE,
        category,
        number,
        log_core::Severity::Warning,
        message,
    )
    .module("memory")
    .build();
    logger.inner().emit(diagnostic);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logger::{ffi_logger_create, ffi_logger_destroy};

    fn read_stats(tag: u8) -> MemoryStats {
        let mut out = MemoryStats::default();
        // SAFETY: `out` is a valid, writable `MemoryStats`.
        let ok = unsafe { ffi_memory_tag_stats(tag, &mut out) };
        assert!(ok);
        out
    }

    #[test]
    fn tag_bytes_agree_with_the_memory_crate() {
        for tag in MemoryTag::ALL {
            assert_eq!(tag_from_code(tag_code(tag)), Some(tag));
            assert_eq!(tag_name(tag).to_str(), Ok(tag.name()));
        }
        assert_eq!(MemoryTag::ALL.len(), usize::from(FFI_MEMORY_TAG_COUNT));
        assert_eq!(tag_from_code(FFI_MEMORY_TAG_COUNT), None);
        assert_eq!(tag_from_code(FFI_MEMORY_TAG_UNKNOWN), None);
    }

    #[test]
    fn tag_names_are_static_c_strings() {
        let ptr = ffi_memory_tag_name(FFI_MEMORY_TAG_RENDERING);
        assert!(!ptr.is_null());
        // SAFETY: a non-null result points to a static NUL-terminated string.
        let name = unsafe { CStr::from_ptr(ptr) };
        assert_eq!(name.to_str(), Ok("Rendering"));
        assert!(ffi_memory_tag_name(FFI_MEMORY_TAG_COUNT).is_null());
    }

    #[test]
    fn unknown_tags_are_rejected_and_change_nothing() {
        let mut stats = MemoryStats::default();
        let mut budget = 7u64;
        assert!(!ffi_memory_record_alloc(FFI_MEMORY_TAG_COUNT, 1));
        assert!(!ffi_memory_record_dealloc(255, 1));
        assert!(!ffi_memory_set_budget(200, 1));
        assert!(!ffi_memory_clear_budget(200));
        assert!(!ffi_memory_is_over_budget(200));
        // SAFETY: both pointers are valid and writable.
        unsafe {
            assert!(!ffi_memory_tag_stats(FFI_MEMORY_TAG_COUNT, &mut stats));
            assert!(!ffi_memory_budget(FFI_MEMORY_TAG_COUNT, &mut budget));
        }
        assert_eq!(stats, MemoryStats::default());
        assert_eq!(budget, 7);
    }

    #[test]
    fn registered_memory_shows_up_in_the_tag_stats() {
        // Only this test uses `FFI_MEMORY_TAG_PHYSICS`.
        let tag = FFI_MEMORY_TAG_PHYSICS;
        let before = read_stats(tag);

        assert!(ffi_memory_record_alloc(tag, 4096));
        let during = read_stats(tag);
        assert_eq!(during.current_bytes, before.current_bytes + 4096);
        assert_eq!(during.allocations, before.allocations + 1);
        assert!(during.peak_bytes >= during.current_bytes);

        assert!(ffi_memory_record_dealloc(tag, 4096));
        let after = read_stats(tag);
        assert_eq!(after.current_bytes, before.current_bytes);
        assert_eq!(after.deallocations, before.deallocations + 1);
        assert_eq!(after.reallocations, 0);
    }

    #[test]
    fn the_total_counts_every_tag() {
        // Only this test uses `FFI_MEMORY_TAG_GENERAL`.
        let mut before = MemoryStats::default();
        let mut after = MemoryStats::default();
        // SAFETY: both pointers are valid and writable.
        unsafe { assert!(ffi_memory_total_stats(&mut before)) };
        assert!(ffi_memory_record_alloc(FFI_MEMORY_TAG_GENERAL, 100));
        // SAFETY: as above.
        unsafe { assert!(ffi_memory_total_stats(&mut after)) };
        assert!(ffi_memory_record_dealloc(FFI_MEMORY_TAG_GENERAL, 100));

        // Other tests allocate in parallel, so only what cannot go down is checked.
        let grown = after.total_allocated_bytes - before.total_allocated_bytes;
        assert!(grown >= 100);
        assert!(after.allocations > before.allocations);
    }

    #[test]
    fn budgets_can_be_set_read_checked_and_cleared() {
        // Only this test uses `FFI_MEMORY_TAG_AUDIO`.
        let tag = FFI_MEMORY_TAG_AUDIO;
        let mut budget = 0u64;
        // SAFETY: `budget` is valid and writable.
        unsafe { assert!(!ffi_memory_budget(tag, &mut budget)) };

        assert!(ffi_memory_set_budget(tag, 1024));
        // SAFETY: as above.
        unsafe { assert!(ffi_memory_budget(tag, &mut budget)) };
        assert_eq!(budget, 1024);
        assert!(!ffi_memory_is_over_budget(tag));

        assert!(ffi_memory_record_alloc(tag, 2048));
        assert!(ffi_memory_is_over_budget(tag));

        assert!(ffi_memory_record_dealloc(tag, 2048));
        assert!(!ffi_memory_is_over_budget(tag));
        assert!(ffi_memory_clear_budget(tag));
        // SAFETY: as above.
        unsafe { assert!(!ffi_memory_budget(tag, &mut budget)) };
    }

    #[test]
    fn the_report_is_text_the_caller_frees() {
        // Only this test uses `FFI_MEMORY_TAG_NETWORKING`.
        let tag = FFI_MEMORY_TAG_NETWORKING;
        assert!(ffi_memory_record_alloc(tag, 512));

        let ptr = ffi_memory_report_string();
        assert!(!ptr.is_null());
        // SAFETY: `ptr` was just returned and is freed once below.
        let c_text = unsafe { CStr::from_ptr(ptr) };
        let text = c_text.to_str().unwrap().to_owned();
        // SAFETY: `ptr` came from `ffi_memory_report_string` and is freed once.
        unsafe { crate::ffi_free_string(ptr) };

        assert!(text.starts_with("total: "), "{text}");
        assert!(text.contains("Networking: 512 B in use"), "{text}");
        assert!(ffi_memory_record_dealloc(tag, 512));
    }

    #[test]
    fn leaks_and_budget_overruns_become_diagnostics() {
        // Only this test uses `FFI_MEMORY_TAG_UI`.
        let tag = FFI_MEMORY_TAG_UI;
        assert!(ffi_memory_set_budget(tag, 100));
        assert!(ffi_memory_record_alloc(tag, 1000));

        let logger = ffi_logger_create();
        assert!(!logger.is_null());
        // SAFETY: `logger` was just created and is destroyed below.
        let emitted = unsafe { ffi_memory_report_diagnostics(logger) };
        // This tag alone is a leak and over its budget; other tests running in
        // parallel may add more.
        assert!(emitted >= 2, "{emitted}");

        assert!(ffi_memory_record_dealloc(tag, 1000));
        assert!(ffi_memory_clear_budget(tag));
        // SAFETY: `logger` is live and not used afterwards.
        unsafe { ffi_logger_destroy(logger) };
    }

    #[test]
    fn a_null_logger_emits_nothing() {
        // SAFETY: null is documented as "emit nothing".
        let emitted = unsafe { ffi_memory_report_diagnostics(std::ptr::null_mut()) };
        assert_eq!(emitted, 0);
    }
}
