#include "cv-ffi/Memory.hpp"

#include "cv-ffi/Logger.hpp"
#include "ffi.h"

namespace cv_ffi::memory {

namespace {

// Tags are passed to Rust as their raw byte, so they must match the header.
static_assert(static_cast<std::uint8_t>(Tag::General) == FFI_MEMORY_TAG_GENERAL);
static_assert(static_cast<std::uint8_t>(Tag::Rendering) == FFI_MEMORY_TAG_RENDERING);
static_assert(static_cast<std::uint8_t>(Tag::Assets) == FFI_MEMORY_TAG_ASSETS);
static_assert(static_cast<std::uint8_t>(Tag::Ecs) == FFI_MEMORY_TAG_ECS);
static_assert(static_cast<std::uint8_t>(Tag::Physics) == FFI_MEMORY_TAG_PHYSICS);
static_assert(static_cast<std::uint8_t>(Tag::Audio) == FFI_MEMORY_TAG_AUDIO);
static_assert(static_cast<std::uint8_t>(Tag::Scripting) == FFI_MEMORY_TAG_SCRIPTING);
static_assert(static_cast<std::uint8_t>(Tag::Ui) == FFI_MEMORY_TAG_UI);
static_assert(static_cast<std::uint8_t>(Tag::Networking) == FFI_MEMORY_TAG_NETWORKING);
static_assert(static_cast<std::uint8_t>(Tag::Tools) == FFI_MEMORY_TAG_TOOLS);
static_assert(kTagCount == static_cast<std::size_t>(FFI_MEMORY_TAG_COUNT));

std::uint8_t Code(Tag tag) noexcept
{
    return static_cast<std::uint8_t>(tag);
}

Stats FromRaw(const ::MemoryStats& raw) noexcept
{
    Stats stats;
    stats.currentBytes = raw.current_bytes;
    stats.peakBytes = raw.peak_bytes;
    stats.totalAllocatedBytes = raw.total_allocated_bytes;
    stats.allocations = raw.allocations;
    stats.deallocations = raw.deallocations;
    stats.reallocations = raw.reallocations;
    return stats;
}

} // namespace

std::string_view TagName(Tag tag)
{
    const char* name = ffi_memory_tag_name(Code(tag));
    if (name == nullptr) {
        return "Unknown";
    }
    return name;
}

Stats TotalStats()
{
    ::MemoryStats raw{};
    // Fails only when a Rust panic was caught at the boundary; report zeros then.
    if (!ffi_memory_total_stats(&raw)) {
        return {};
    }
    return FromRaw(raw);
}

Stats TagStats(Tag tag)
{
    ::MemoryStats raw{};
    // Cannot fail for a Tag: every enumerator is a valid tag byte (see the
    // static_asserts above). A caught Rust panic is reported as zeros.
    if (!ffi_memory_tag_stats(Code(tag), &raw)) {
        return {};
    }
    return FromRaw(raw);
}

void RecordAlloc(Tag tag, std::uint64_t bytes)
{
    static_cast<void>(ffi_memory_record_alloc(Code(tag), bytes));
}

void RecordDealloc(Tag tag, std::uint64_t bytes)
{
    static_cast<void>(ffi_memory_record_dealloc(Code(tag), bytes));
}

void SetBudget(Tag tag, std::uint64_t bytes)
{
    static_cast<void>(ffi_memory_set_budget(Code(tag), bytes));
}

void ClearBudget(Tag tag)
{
    static_cast<void>(ffi_memory_clear_budget(Code(tag)));
}

std::optional<std::uint64_t> Budget(Tag tag)
{
    std::uint64_t bytes = 0;
    if (!ffi_memory_budget(Code(tag), &bytes)) {
        return std::nullopt;
    }
    return bytes;
}

bool IsOverBudget(Tag tag)
{
    return ffi_memory_is_over_budget(Code(tag));
}

std::string Report()
{
    char* text = ffi_memory_report_string();
    if (text == nullptr) {
        return {};
    }
    std::string result(text);
    ffi_free_string(text);
    return result;
}

std::uint32_t EmitReport(Logger& logger)
{
    // ffi_memory_report_diagnostics treats null as "emit nothing", but an
    // invalid Logger has no handle to pass anyway.
    if (!logger.IsValid()) {
        return 0;
    }
    return ffi_memory_report_diagnostics(logger.GetNativeHandle());
}

TrackedAllocation::TrackedAllocation(Tag tag, std::uint64_t bytes) : m_tag(tag), m_bytes(bytes), m_active(true)
{
    RecordAlloc(m_tag, m_bytes);
}

TrackedAllocation::~TrackedAllocation()
{
    Release();
}

TrackedAllocation::TrackedAllocation(TrackedAllocation&& other) noexcept
    : m_tag(other.m_tag), m_bytes(other.m_bytes), m_active(other.m_active)
{
    other.m_bytes = 0;
    other.m_active = false;
}

TrackedAllocation& TrackedAllocation::operator=(TrackedAllocation&& other) noexcept
{
    if (this != &other) {
        Release();
        m_tag = other.m_tag;
        m_bytes = other.m_bytes;
        m_active = other.m_active;
        other.m_bytes = 0;
        other.m_active = false;
    }
    return *this;
}

void TrackedAllocation::Resize(std::uint64_t bytes)
{
    if (!m_active) {
        return;
    }
    RecordDealloc(m_tag, m_bytes);
    RecordAlloc(m_tag, bytes);
    m_bytes = bytes;
}

void TrackedAllocation::Release() noexcept
{
    if (m_active) {
        RecordDealloc(m_tag, m_bytes);
        m_active = false;
        m_bytes = 0;
    }
}

TrackingResource::TrackingResource(Tag tag, std::pmr::memory_resource* upstream) : m_tag(tag), m_upstream(upstream) {}

void* TrackingResource::do_allocate(std::size_t bytes, std::size_t alignment)
{
    // Register only after the upstream succeeded: a throw leaves nothing counted.
    void* pointer = m_upstream->allocate(bytes, alignment);
    RecordAlloc(m_tag, static_cast<std::uint64_t>(bytes));
    return pointer;
}

void TrackingResource::do_deallocate(void* pointer, std::size_t bytes, std::size_t alignment)
{
    m_upstream->deallocate(pointer, bytes, alignment);
    RecordDealloc(m_tag, static_cast<std::uint64_t>(bytes));
}

bool TrackingResource::do_is_equal(const std::pmr::memory_resource& other) const noexcept
{
    // Memory may only be released through the resource that registered it, under
    // its tag, so two resources are equal only when they are the same object.
    return this == &other;
}

} // namespace cv_ffi::memory
