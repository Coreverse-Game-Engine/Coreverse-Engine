#pragma once

#include <cstddef>
#include <cstdint>
#include <memory_resource>
#include <optional>
#include <string>
#include <string_view>

// See Logger.hpp for why this is forward-declared rather than pulled in via
// ffi.h.
namespace cv_ffi {

class Logger;

} // namespace cv_ffi

namespace cv_ffi::memory {

/// Mirrors the `FFI_MEMORY_TAG_*` constants of `ffi::memory`
/// (engine/rust/crates/ffi/src/memory.rs), i.e. `memory::MemoryTag`: the
/// engine system a block of memory belongs to. Keep the numbering in sync with
/// the Rust side; `static_assert`s in Memory.cpp pin it.
enum class Tag : std::uint8_t {
    /// Memory that belongs to no particular system.
    General = 0,
    Rendering = 1,
    Assets = 2,
    Ecs = 3,
    Physics = 4,
    Audio = 5,
    Scripting = 6,
    Ui = 7,
    Networking = 8,
    Tools = 9,
};

/// The number of tags.
inline constexpr std::size_t kTagCount = 10;

/// Allocation counters of one tag, or of all tags together (`memory::MemoryStats`).
struct Stats {
    /// The bytes registered and not yet released.
    std::uint64_t currentBytes = 0;
    /// The highest value `currentBytes` has reached.
    std::uint64_t peakBytes = 0;
    /// The sum of all bytes ever registered; it only grows.
    std::uint64_t totalAllocatedBytes = 0;
    std::uint64_t allocations = 0;
    std::uint64_t deallocations = 0;
    /// Always 0 for memory registered from C++, which has no resize call.
    std::uint64_t reallocations = 0;

    /// The allocations that have not been released yet.
    [[nodiscard]] std::uint64_t LiveAllocations() const noexcept
    {
        return allocations > deallocations ? allocations - deallocations : 0;
    }
};

/// The name of a tag ("Rendering", ...), for logs and reports.
[[nodiscard]] std::string_view TagName(Tag tag);

/// The counters of all tags together (wraps `ffi_memory_total_stats`).
[[nodiscard]] Stats TotalStats();

/// The counters of one tag (wraps `ffi_memory_tag_stats`).
[[nodiscard]] Stats TagStats(Tag tag);

/// Registers an allocation of `bytes` bytes under `tag` in the process-wide
/// tracker (wraps `ffi_memory_record_alloc`). The tracker only counts: pair
/// every call with RecordDealloc() when the memory is freed, with the same tag.
/// Prefer TrackedAllocation or TrackingResource, which do the pairing for you.
void RecordAlloc(Tag tag, std::uint64_t bytes);

/// Registers that `bytes` bytes registered under `tag` were freed. Freeing more
/// than was registered stops the counter at zero.
void RecordDealloc(Tag tag, std::uint64_t bytes);

/// Sets the budget of `tag`. A budget is advisory: it changes IsOverBudget()
/// and the report, nothing is refused.
void SetBudget(Tag tag, std::uint64_t bytes);

/// Removes the budget of `tag`.
void ClearBudget(Tag tag);

/// The budget of `tag`, unset when it has none.
[[nodiscard]] std::optional<std::uint64_t> Budget(Tag tag);

/// Whether `tag` has a budget and uses more than it.
[[nodiscard]] bool IsOverBudget(Tag tag);

/// The report of the tracker as text: a `total:` line and one line per tag that
/// was used or has a budget (wraps `ffi_memory_report_string`).
[[nodiscard]] std::string Report();

/// Emits a warning through `logger` for every tag that still has bytes in use
/// (`CV-MEMORY-LEAK-<n>`) and for every tag over its budget
/// (`CV-MEMORY-BUDGET-<n>`), where `<n>` is the tag number plus one. Returns the
/// number of diagnostics emitted: 0 when everything is freed and within budget,
/// and also when `logger` is not valid.
///
/// Call it when the program shuts down or a level ends: only then is "still in
/// use" a leak. Wraps `ffi_memory_report_diagnostics`.
std::uint32_t EmitReport(Logger& logger);

/// Registers a block of memory under a tag for as long as this object lives.
///
/// The constructor calls RecordAlloc(), the destructor RecordDealloc(), so a
/// buffer that is owned next to a TrackedAllocation is counted exactly while it
/// exists. Move-only; a moved-from object registers nothing.
class TrackedAllocation
{
public:
    TrackedAllocation() noexcept = default;
    TrackedAllocation(Tag tag, std::uint64_t bytes);
    ~TrackedAllocation();

    TrackedAllocation(const TrackedAllocation&) = delete;
    TrackedAllocation& operator=(const TrackedAllocation&) = delete;

    TrackedAllocation(TrackedAllocation&& other) noexcept;
    TrackedAllocation& operator=(TrackedAllocation&& other) noexcept;

    /// Registers the new size instead of the old one (the old registration is
    /// released first). Does nothing on an object that registers nothing.
    void Resize(std::uint64_t bytes);

    /// Releases the registration early. Safe to call more than once.
    void Release() noexcept;

    /// Whether a registration is currently held.
    [[nodiscard]] bool IsActive() const noexcept
    {
        return m_active;
    }

    /// The registered size (0 when inactive).
    [[nodiscard]] std::uint64_t Bytes() const noexcept
    {
        return m_bytes;
    }

    [[nodiscard]] Tag GetTag() const noexcept
    {
        return m_tag;
    }

private:
    Tag m_tag = Tag::General;
    std::uint64_t m_bytes = 0;
    bool m_active = false;
};

/// A `std::pmr::memory_resource` that registers every allocation under a tag and
/// forwards to an upstream resource: the allocation hook for C++ code. Containers
/// that take a polymorphic allocator are tracked by handing them this resource:
///
///     cv_ffi::memory::TrackingResource resource(cv_ffi::memory::Tag::Rendering);
///     std::pmr::vector<Vertex> vertices(&resource);
///
/// Nothing is replaced globally: `operator new` and `operator delete` are left
/// alone, and only memory that goes through a TrackingResource is counted. The
/// resource must outlive everything allocated from it. It is not thread-safe in
/// itself beyond what the upstream resource guarantees (the counters are).
class TrackingResource final : public std::pmr::memory_resource
{
public:
    explicit TrackingResource(Tag tag, std::pmr::memory_resource* upstream = std::pmr::get_default_resource());

    TrackingResource(const TrackingResource&) = delete;
    TrackingResource& operator=(const TrackingResource&) = delete;

    [[nodiscard]] Tag GetTag() const noexcept
    {
        return m_tag;
    }

private:
    void* do_allocate(std::size_t bytes, std::size_t alignment) override;
    void do_deallocate(void* pointer, std::size_t bytes, std::size_t alignment) override;
    [[nodiscard]] bool do_is_equal(const std::pmr::memory_resource& other) const noexcept override;

    Tag m_tag;
    std::pmr::memory_resource* m_upstream;
};

} // namespace cv_ffi::memory
