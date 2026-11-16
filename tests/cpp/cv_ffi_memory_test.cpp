// Memory (M5) proof: cv-ffi's memory wrappers against the real Rust library --
// the per-tag counters, TrackedAllocation, the TrackingResource allocation hook
// for std::pmr containers, budgets, the text report and the leak/budget report
// as log diagnostics.
//
// Every sub-test uses its own tag, so the checks on the counters of one tag
// cannot be disturbed by another.

#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <iterator>
#include <memory_resource>
#include <optional>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

#include "cv-ffi/Logger.hpp"
#include "cv-ffi/Memory.hpp"

namespace {

namespace memory = cv_ffi::memory;

int g_failures = 0;

void Check(bool ok, std::string_view what)
{
    if (!ok) {
        std::fprintf(stderr, "FAIL: %.*s\n", static_cast<int>(what.size()), what.data());
        ++g_failures;
    }
}

void TestTagNames()
{
    const std::pair<memory::Tag, std::string_view> expected[] = {
        {memory::Tag::General, "General"},
        {memory::Tag::Rendering, "Rendering"},
        {memory::Tag::Assets, "Assets"},
        {memory::Tag::Ecs, "Ecs"},
        {memory::Tag::Physics, "Physics"},
        {memory::Tag::Audio, "Audio"},
        {memory::Tag::Scripting, "Scripting"},
        {memory::Tag::Ui, "Ui"},
        {memory::Tag::Networking, "Networking"},
        {memory::Tag::Tools, "Tools"},
    };
    Check(std::size(expected) == memory::kTagCount, "every tag has a name in this test");
    for (const auto& entry : expected) {
        Check(memory::TagName(entry.first) == entry.second, "the tag name matches the Rust side");
    }
}

void TestRegisteredMemoryShowsInTheStats()
{
    constexpr memory::Tag tag = memory::Tag::Physics;
    const memory::Stats before = memory::TagStats(tag);

    memory::RecordAlloc(tag, 4096);
    const memory::Stats during = memory::TagStats(tag);
    Check(during.currentBytes == before.currentBytes + 4096, "RecordAlloc raises the bytes in use");
    Check(during.allocations == before.allocations + 1, "RecordAlloc counts one allocation");
    Check(during.peakBytes >= during.currentBytes, "the peak is at least the current value");
    Check(during.LiveAllocations() >= 1, "there is a live allocation");

    memory::RecordDealloc(tag, 4096);
    const memory::Stats after = memory::TagStats(tag);
    Check(after.currentBytes == before.currentBytes, "RecordDealloc brings the bytes back");
    Check(after.deallocations == before.deallocations + 1, "RecordDealloc counts one deallocation");

    const memory::Stats total = memory::TotalStats();
    Check(total.totalAllocatedBytes >= 4096, "the total saw the allocation");
}

void TestTrackedAllocation()
{
    constexpr memory::Tag tag = memory::Tag::Audio;
    const std::uint64_t before = memory::TagStats(tag).currentBytes;

    {
        memory::TrackedAllocation block(tag, 100);
        Check(block.IsActive() && block.Bytes() == 100 && block.GetTag() == tag, "the registration is visible");
        Check(memory::TagStats(tag).currentBytes == before + 100, "the constructor registers the bytes");

        block.Resize(300);
        Check(memory::TagStats(tag).currentBytes == before + 300, "Resize registers the new size instead");

        // Moving transfers the registration; the moved-from object holds none.
        memory::TrackedAllocation moved(std::move(block));
        // Looking at the moved-from object is the point of this check.
        Check(!block.IsActive(), "the moved-from object is inactive"); // NOLINT(bugprone-use-after-move)
        Check(moved.IsActive() && moved.Bytes() == 300, "the moved-to object holds the registration");
        Check(memory::TagStats(tag).currentBytes == before + 300, "a move registers nothing twice");
    }
    Check(memory::TagStats(tag).currentBytes == before, "the destructor releases the bytes");

    memory::TrackedAllocation early(tag, 50);
    early.Release();
    early.Release();
    Check(memory::TagStats(tag).currentBytes == before, "Release is idempotent");
}

void TestTrackingResource()
{
    constexpr memory::Tag tag = memory::Tag::Rendering;
    const std::uint64_t before = memory::TagStats(tag).currentBytes;

    memory::TrackingResource resource(tag);
    Check(resource.GetTag() == tag, "the resource knows its tag");
    Check(!resource.is_equal(*std::pmr::get_default_resource()), "it is not equal to the default resource");
    {
        std::pmr::vector<int> values(&resource);
        values.reserve(1000);
        Check(
            memory::TagStats(tag).currentBytes >= before + 1000 * sizeof(int), "a pmr container's memory is registered"
        );
    }
    Check(memory::TagStats(tag).currentBytes == before, "freeing the container releases it");
}

void TestBudgets()
{
    constexpr memory::Tag tag = memory::Tag::Ui;
    Check(!memory::Budget(tag).has_value(), "no budget to start with");

    memory::SetBudget(tag, 1024);
    Check(memory::Budget(tag) == std::optional<std::uint64_t>(1024), "the budget can be read back");
    Check(!memory::IsOverBudget(tag), "nothing used, not over budget");

    memory::RecordAlloc(tag, 2048);
    Check(memory::IsOverBudget(tag), "using more than the budget is over budget");

    memory::RecordDealloc(tag, 2048);
    Check(!memory::IsOverBudget(tag), "freeing brings it back within budget");

    memory::ClearBudget(tag);
    Check(!memory::Budget(tag).has_value(), "the budget can be removed");
}

void TestReport()
{
    constexpr memory::Tag tag = memory::Tag::Networking;
    memory::RecordAlloc(tag, 512);

    const std::string report = memory::Report();
    Check(report.starts_with("total: "), "the report starts with the total");
    Check(report.contains("Networking: 512 B in use"), "the report lists the tag with its bytes");

    memory::RecordDealloc(tag, 512);
}

void TestLeaksBecomeDiagnostics()
{
    constexpr memory::Tag tag = memory::Tag::Tools;
    auto logger = cv_ffi::Logger::Create();
    Check(logger.has_value(), "a Logger can be created");
    if (!logger) {
        return;
    }

    memory::RecordAlloc(tag, 512);
    // This tag alone is a leak; other tags may add more.
    Check(memory::EmitReport(*logger) >= 1, "a leaking tag is reported");
    memory::RecordDealloc(tag, 512);

    cv_ffi::Logger invalid;
    Check(memory::EmitReport(invalid) == 0, "an invalid Logger emits nothing");
}

} // namespace

int main()
{
    TestTagNames();
    TestRegisteredMemoryShowsInTheStats();
    TestTrackedAllocation();
    TestTrackingResource();
    TestBudgets();
    TestReport();
    TestLeaksBecomeDiagnostics();

    if (g_failures != 0) {
        std::fprintf(stderr, "%d check(s) failed\n", g_failures);
        return 1;
    }
    std::printf("cv_ffi_memory_test: all checks passed\n");
    return 0;
}
