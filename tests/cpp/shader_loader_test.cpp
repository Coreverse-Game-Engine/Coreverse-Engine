// Phase 5 / renderer M2 proof: RenderDevice::CreateShaderModule() and renderer::ShaderLoader --
// everything about getting shader bytes into a ShaderModule that does not need a window:
//
//   * a module is a move-only RAII handle, one module holds both stages of the built-in shader;
//   * CreateShaderModule() rejects data that is not SPIR-V (header, malformed instruction stream)
//     before the driver sees it;
//   * the loader prefers `shaders/<name>.spv` from its reader (here: an in-memory one), falls back
//     to the built-in module of the same name and says why (missing, too small, not a multiple
//     of 4, wrong magic number, malformed stream), reports an error for a name that is neither,
//     and refuses names that could leave the shaders directory.
//
// The VFS-backed reader is tested against the real Rust VFS in renderer_vfs_shader_test; the
// pipeline side (entry points, stage mix-ups) in graphics_pass_test. Run with the validation layer
// so that anything the renderer lets through that it should not have shows up as output.

#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <map>
#include <memory>
#include <string>
#include <utility>
#include <vector>

#include "renderer/BuiltinShaders.hpp"
#include "renderer/RenderDeviceFactory.hpp"
#include "renderer/ShaderLoader.hpp"

namespace {

bool Fail(const char* what)
{
    std::fprintf(stderr, "FAIL: %s\n", what);
    return false;
}

bool Fail(const std::string& what)
{
    return Fail(what.c_str());
}

std::vector<std::byte> ToBytes(std::span<const std::uint32_t> words)
{
    std::vector<std::byte> bytes(words.size_bytes());
    std::memcpy(bytes.data(), words.data(), bytes.size());
    return bytes;
}

bool Contains(const std::string& text, const char* needle)
{
    return text.find(needle) != std::string::npos;
}

/// An in-memory "VFS": path -> bytes. Also records every path it was asked for.
struct FakeFiles {
    std::map<std::string, std::vector<std::byte>> files;
    mutable std::vector<std::string> requested;

    renderer::ShaderBinaryReader Reader() const
    {
        return [this](std::string_view path) -> std::expected<std::vector<std::byte>, std::string> {
            requested.emplace_back(path);
            const auto it = files.find(std::string(path));
            if (it == files.end()) {
                return std::unexpected("not found: " + std::string(path));
            }
            return it->second;
        };
    }
};

bool TestModules(renderer::RenderDevice& device)
{
    const auto builtin = renderer::ColoredMeshShader();
    if (builtin.empty() || renderer::FindBuiltinShader(renderer::kColoredMeshShaderName).data() != builtin.data()) {
        return Fail(
            "the built-in colored_mesh shader is missing or FindBuiltinShader disagrees with ColoredMeshShader"
        );
    }
    if (!renderer::FindBuiltinShader("no_such_shader").empty() || !renderer::FindBuiltinShader("").empty()) {
        return Fail("FindBuiltinShader returned something for an unknown name");
    }

    auto module = device.CreateShaderModule(builtin);
    if (!module) {
        return Fail("CreateShaderModule(built-in) failed: " + module.error().detail);
    }
    if (!module->IsValid() || module->GetNativeHandle() == nullptr) {
        return Fail("a fresh module is not valid");
    }
    renderer::ShaderModule moved = std::move(*module);
    if (module->IsValid() || !moved.IsValid()) {
        return Fail("move left the source valid or the target invalid");
    }
    renderer::ShaderModule assigned;
    assigned = std::move(moved);
    if (moved.IsValid() || !assigned.IsValid()) {
        return Fail("move assignment broke ownership");
    }
    if (renderer::ShaderModule{}.IsValid()) {
        return Fail("a default-constructed module is valid");
    }

    // Not SPIR-V: all of these must be refused by the renderer itself.
    const std::vector<std::uint32_t> empty;
    if (device.CreateShaderModule(empty)) {
        return Fail("an empty word list was accepted");
    }
    std::vector<std::uint32_t> tooShort(builtin.begin(), builtin.begin() + 4);
    if (device.CreateShaderModule(tooShort)) {
        return Fail("a binary shorter than the header was accepted");
    }
    std::vector<std::uint32_t> badMagic(builtin.begin(), builtin.end());
    badMagic[0] = 0x12345678;
    if (device.CreateShaderModule(badMagic)) {
        return Fail("a binary with the wrong magic number was accepted");
    }
    // Valid header, then an instruction that claims zero words / runs past the end.
    std::vector<std::uint32_t> zeroWordCount(builtin.begin(), builtin.begin() + 5);
    zeroWordCount.push_back(0x00000000);
    if (device.CreateShaderModule(zeroWordCount)) {
        return Fail("an instruction with a word count of 0 was accepted");
    }
    std::vector<std::uint32_t> overrun(builtin.begin(), builtin.begin() + 5);
    overrun.push_back((100u << 16) | 15u); // OpEntryPoint claiming 100 words, none follow
    if (device.CreateShaderModule(overrun)) {
        return Fail("an instruction running past the end of the binary was accepted");
    }
    return true;
}

bool TestLoader(renderer::RenderDevice& device)
{
    const auto builtinWords = renderer::ColoredMeshShader();
    const std::vector<std::byte> goodBytes = ToBytes(builtinWords);

    // --- no reader: built-in only, no fallback reason (nothing failed) ---
    {
        const renderer::ShaderLoader loader(device);
        auto shader = loader.Load(renderer::kColoredMeshShaderName);
        if (!shader) {
            return Fail("loader without a reader could not load the built-in shader: " + shader.error().detail);
        }
        if (shader->origin != renderer::ShaderOrigin::Builtin || !shader->fallbackReason.empty() ||
            !shader->module.IsValid()) {
            return Fail("loader without a reader: wrong origin / fallback reason / module");
        }
        if (loader.Load("does_not_exist")) {
            return Fail("loader without a reader invented a shader");
        }
    }

    // --- reader with a good file: Loaded, asked for exactly shaders/<name>.spv ---
    FakeFiles fake;
    fake.files["shaders/colored_mesh.spv"] = goodBytes;
    fake.files["shaders/other.spv"] = goodBytes;   // no built-in of that name: only the file can provide it
    fake.files["shaders/lit/pbr.spv"] = goodBytes; // sub-folder
    const renderer::ShaderLoader loader(device, fake.Reader());
    {
        auto shader = loader.Load("colored_mesh");
        if (!shader) {
            return Fail("loader failed on a good file: " + shader.error().detail);
        }
        if (shader->origin != renderer::ShaderOrigin::Loaded || !shader->fallbackReason.empty()) {
            return Fail("a good file was not reported as Loaded");
        }
        if (fake.requested.size() != 1 || fake.requested[0] != "shaders/colored_mesh.spv") {
            return Fail("the reader was not asked for exactly shaders/colored_mesh.spv");
        }
    }
    {
        auto other = loader.Load("other");
        if (!other || other->origin != renderer::ShaderOrigin::Loaded) {
            return Fail("a file-only shader (no built-in of that name) could not be loaded");
        }
        auto nested = loader.Load("lit/pbr");
        if (!nested || nested->origin != renderer::ShaderOrigin::Loaded) {
            return Fail("a shader in a sub-folder could not be loaded");
        }
        if (fake.requested.back() != "shaders/lit/pbr.spv") {
            return Fail("sub-folder path was not shaders/lit/pbr.spv");
        }
    }

    // --- a bad file falls back to the built-in module and says why ---
    struct Case {
        const char* what;
        std::vector<std::byte> bytes; // empty + `remove` = the file does not exist
        bool remove;
        const char* reasonContains;
    };
    std::vector<std::byte> oddSize = goodBytes;
    oddSize.push_back(std::byte{0});
    std::vector<std::byte> wrongMagic = goodBytes;
    wrongMagic[0] = std::byte{0xAA};
    std::vector<std::byte> malformed(goodBytes.begin(), goodBytes.begin() + 20); // header ...
    malformed.insert(
        malformed.end(), {std::byte{0}, std::byte{0}, std::byte{0}, std::byte{0}}
    ); // ... then word count 0
    const Case cases[] = {
        {"missing file", {}, true, "not found"},
        {"tiny file", std::vector<std::byte>(7), false, "too small"},
        {"size not a multiple of 4", oddSize, false, "multiple of 4"},
        {"wrong magic number", wrongMagic, false, "magic"},
        {"malformed instruction stream", malformed, false, "malformed"},
    };
    for (const Case& c : cases) {
        FakeFiles broken;
        if (!c.remove) {
            broken.files["shaders/colored_mesh.spv"] = c.bytes;
        }
        const renderer::ShaderLoader brokenLoader(device, broken.Reader());
        auto shader = brokenLoader.Load(renderer::kColoredMeshShaderName);
        if (!shader) {
            return Fail(std::string(c.what) + ": no fallback to the built-in shader: " + shader.error().detail);
        }
        if (shader->origin != renderer::ShaderOrigin::Builtin || !shader->module.IsValid()) {
            return Fail(std::string(c.what) + ": expected a valid built-in module");
        }
        if (!Contains(shader->fallbackReason, "shaders/colored_mesh.spv") ||
            !Contains(shader->fallbackReason, c.reasonContains)) {
            return Fail(
                std::string(c.what) + ": fallback reason does not name the file and the problem: '" +
                shader->fallbackReason + "'"
            );
        }
        // A bad file for a name without a built-in is an error that carries the reason.
        broken.files["shaders/nothing_builtin.spv"] = c.bytes.empty() ? std::vector<std::byte>(3) : c.bytes;
        auto nothing = brokenLoader.Load("nothing_builtin");
        if (nothing) {
            return Fail(std::string(c.what) + ": a bad file with no built-in of that name was accepted");
        }
        if (!Contains(nothing.error().detail, "nothing_builtin") || !Contains(nothing.error().detail, "no built-in")) {
            return Fail(
                std::string(c.what) + ": error does not name the shader / say there is no built-in: '" +
                nothing.error().detail + "'"
            );
        }
    }

    // --- names that could leave the shaders directory (or are not names at all) ---
    const char* badNames[] = {"", "/abs", "dir/", "../x", "a/../b", "a/./b", "a//b", ".", "..", "a\\b", "c:x", "x:"};
    const std::size_t requestedBefore = fake.requested.size();
    for (const char* name : badNames) {
        if (loader.Load(name)) {
            return Fail(std::string("invalid shader name accepted: '") + name + "'");
        }
    }
    if (fake.requested.size() != requestedBefore) {
        return Fail("the reader was called for an invalid shader name");
    }
    if (renderer::ShaderLoader::PathFor("lit/pbr") != "shaders/lit/pbr.spv") {
        return Fail("PathFor is wrong");
    }
    return true;
}

} // namespace

int main()
{
    auto deviceResult = renderer::CreateRenderDevice(renderer::GraphicsAPI::Vulkan);
    if (!deviceResult) {
        std::fprintf(stderr, "CreateRenderDevice failed: %s\n", deviceResult.error().detail.c_str());
        return 1;
    }
    std::unique_ptr<renderer::RenderDevice> device = std::move(*deviceResult);

    // Each section keeps its modules in its own scope: they are all gone before Shutdown().
    const bool ok = TestModules(*device) && TestLoader(*device);
    device->Shutdown();
    if (!ok) {
        return 1;
    }
    std::printf("shader loader OK\n");
    return 0;
}
