# Rust and C++ FFI

The `ffi` crate exposes a C ABI; `cbindgen` generates `ffi.h` from it at build time (`cmake/FfiHeader.cmake`) and
Corrosion links the static library. The C++ side never calls the raw functions directly: `engine/cpp/ffi-cpp`
(`cv-ffi`) wraps them.

| Area                                          | Rust                           | C++ wrapper                                                                |
|:----------------------------------------------|:-------------------------------|:---------------------------------------------------------------------------|
| Logging                                       | `log-core`, `log-sinks`        | `cv_ffi::Logger`, `cv_ffi::DiagnosticBuilder`, `cv_ffi::RegisterProducer`  |
| Virtual file system                           | `vfs`                          | `cv_ffi::vfs::*`                                                           |
| Platform information and standard directories | `platform`                     | `cv_ffi::platform::Info()`, `cv_ffi::platform::StandardDir()`              |
| Window event loop and input                   | `platform` (`window`, `input`) | `cv_ffi::window::Run()`, `Context`, `Event` (see [Runtime](runtime.md))    |
| Memory counters, budgets and leak report      | `memory`                       | `cv_ffi::memory::*`, `TrackedAllocation`, `TrackingResource`               |
| Build info                                    | `ffi`                          | `cv_ffi::BuildInfoString()`                                                |

## Platform functions

| C function                                                        | C++ wrapper                           | Returns                                                                                                                                                                                          |
|:------------------------------------------------------------------|:--------------------------------------|:-------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `ffi_platform_info(PlatformInfo*)`, `ffi_platform_info_free`      | `platform::Info()`                    | `std::expected<SystemInfo, std::string>`: OS name/version/kernel, logical and physical cores, memory, display server, plus the compile-time platform, architecture, endianness and pointer width |
| `ffi_platform_dir(kind, app)`                                     | `platform::StandardDir(DirKind, app)` | `std::expected<std::string, std::string>`: the config, data, cache, log, temp, home or executable directory of an application (nothing is created)                                               |
| `ffi_platform_last_error_code`, `ffi_platform_last_error_message` | used inside the wrappers              | the reason for the most recent failure on the calling thread                                                                                                                                     |

Conventions, shared with the VFS functions:

* **Enums are `u8`.** The values are the `FFI_PLATFORM_*`, `FFI_ARCH_*`, `FFI_ENDIAN_*`, `FFI_DISPLAY_*` and `FFI_DIR_*`
  constants of `ffi.h`; the C++ enums mirror them, `static_assert`s pin the directory kinds, and an unknown value is
  mapped to `Unknown`.
* **Unknown numbers are `0`.** `physical_cores` and `total_memory_bytes` are `0` when the platform does not report them;
  the C++ side turns that into an unset `std::optional`.
* **Errors use a thread-local slot.** A failing call returns `false` or null; the code (`FFI_PLATFORM_ERROR_*`, `255`
  for none) and a message are read right afterwards on the same thread. A bad `kind` byte, a null or non-UTF-8 `app`
  and an unusable application name are reported as errors, not undefined behavior.
* **Ownership.** Strings are freed with `ffi_free_string`; the three strings inside `PlatformInfo` are freed by
  `ffi_platform_info_free` (which nulls them, so a second call is harmless). `cv_ffi::platform` does both for you.

`cv-ffi` needs only the Rust `ffi` library, not Qt, so CMake builds it whenever `BUILD_EDITOR`, `BUILD_RUNTIME` or
`BUILD_TESTS` is on; its tests (`cv_ffi_platform_test`, `cv_ffi_memory_test` and `cv_ffi_window_test`, label `ffi`) therefore also run in a headless configuration
(`-DBUILD_EDITOR=OFF -DBUILD_RENDERER=OFF -DBUILD_TESTS=ON`). See [Platform layer](platform.md) for what the functions
report on each operating system.

## Memory functions

The `memory` crate keeps one process-wide tracker that counts memory per *tag* (rendering, assets, ecs, ...), with an
optional budget per tag. Rust registers through the tagged arenas; C++ registers what it allocates through these
functions, so one report covers both. Nothing is allocated or freed through them: they only count.

| C function                                                                   | C++ wrapper                                          | Does                                                                                           |
|:-----------------------------------------------------------------------------|:-----------------------------------------------------|:-----------------------------------------------------------------------------------------------|
| `ffi_memory_record_alloc(tag, bytes)`, `ffi_memory_record_dealloc(tag, bytes)` | `memory::RecordAlloc`, `RecordDealloc`, `TrackedAllocation`, `TrackingResource` | register an allocation / its release under a tag                                  |
| `ffi_memory_tag_stats(tag, MemoryStats*)`, `ffi_memory_total_stats(MemoryStats*)` | `memory::TagStats(tag)`, `TotalStats()`          | bytes in use, peak, total allocated, number of allocations and deallocations                   |
| `ffi_memory_set_budget`, `ffi_memory_clear_budget`, `ffi_memory_budget`, `ffi_memory_is_over_budget` | `memory::SetBudget`, `ClearBudget`, `Budget`, `IsOverBudget` | an advisory budget per tag                                          |
| `ffi_memory_report_string()`                                                 | `memory::Report()`                                   | the report as text (free with `ffi_free_string`; the wrapper does)                             |
| `ffi_memory_report_diagnostics(Logger*)`                                     | `memory::EmitReport(Logger&)`                        | one warning per tag that still holds memory and one per tag over budget                        |
| `ffi_memory_tag_name(tag)`                                                   | `memory::TagName(tag)`                               | the name of a tag as a static string (do not free)                                             |

Conventions:

* **Tags are `u8`.** The values are the `FFI_MEMORY_TAG_*` constants of `ffi.h` (`FFI_MEMORY_TAG_COUNT` is `10`);
  `cv_ffi::memory::Tag` mirrors them and `static_assert`s pin the numbers. An unknown tag byte makes the call return
  `false` (or null) and do nothing else; there is no last-error slot because that is the only way these calls fail.
* **Pair every registration.** The tracker counts; it cannot know that memory was freed unless told. Use
  `TrackedAllocation` (RAII, move-only) for a block you own, or hand a `TrackingResource` to a `std::pmr` container:
  `std::pmr::vector<Vertex> vertices(&resource);`. Nothing is replaced globally; `operator new` and `operator delete` are
  untouched, and only memory that goes through these types is counted.
* **Leak and budget diagnostics.** `memory::EmitReport(logger)` emits warnings with the producer `CV-MEMORY` ("Coreverse
  Memory", registered at runtime on first use): `CV-MEMORY-LEAK-<n>` for a tag that still has bytes in use and
  `CV-MEMORY-BUDGET-<n>` for a tag over its budget, where `<n>` is the tag number plus one. Call it at shutdown or at
  the end of a level; only then is "still in use" a leak. It returns how many diagnostics were emitted.
* **The `TrackingAllocator` is not exposed here.** It is a global allocator, and the choice of the global allocator
  belongs to the executable, not to the library C++ links; an executable that installs it reads its numbers from Rust.
  So the numbers above cover registered memory only, not every allocation of the process.

## Window event loop

`ffi_platform_run`, the callback table and the `ffi_platform_ctx_*` functions run the window event loop of the `platform`
crate for C++. They have their own chapter, [Runtime](runtime.md), because the lifetime rules of the context and of
event data matter more than the signatures. They follow the same conventions as above: a function returns `false` on
failure and the reason is read from the thread-local last error, which has two more codes for this area,
`FFI_PLATFORM_ERROR_WINDOW` (`7`, a window operation failed or the window system could not start) and
`FFI_PLATFORM_ERROR_PANIC` (`8`, a Rust panic was caught at the boundary).

## Renderer and logging

The renderer does not depend on this layer. The editor installs a log callback on the render device that forwards
messages (including Vulkan validation output) to the Rust logger.

`math` is *not* exposed through the FFI: the C++ side has its own header-only implementation with an identical
memory layout, so matrices can cross the boundary as plain floats. The golden test keeps both in sync
(see [Math](math.md)).
