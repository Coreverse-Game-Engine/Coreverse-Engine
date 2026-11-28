// Renderer M2 proof: renderer::ShaderLoader with renderer_vfs::MakeVfsShaderReader() against the
// REAL Rust VFS (cv_ffi::vfs, Development mode, a temporary project directory) and a real
// Vulkan device:
//
//   * before the VFS is initialized every read fails, and the loader turns that into a fallback to
//     the built-in shader with a reason - it neither crashes nor fails;
//   * `Assets/shaders/<name>.spv` of the project is found through the VFS (Root::Assets) and wins
//     over the built-in copy; a file for a name without a built-in works too, also in a sub-folder;
//   * a missing, corrupt or truncated file falls back again, with the VFS's message in the reason;
//   * the file the generator's --spv-dir writes is exactly the bytes the built-in array holds.
//
// The loader's own rules (invalid names, error cases, every kind of bad file) are covered without
// a VFS in shader_loader_test.

#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <filesystem>
#include <fstream>
#include <memory>
#include <random>
#include <string>
#include <system_error>
#include <tuple>
#include <vector>

#include "cv-ffi/Vfs.hpp"
#include "renderer/BuiltinShaders.hpp"
#include "renderer/RenderDeviceFactory.hpp"
#include "renderer/ShaderLoader.hpp"
#include "renderer_vfs/VfsShaderReader.hpp"

namespace {

namespace fs = std::filesystem;

// Same pattern as tests/cpp/ffi_vfs_test.cpp's TempDirGuard.
class TempDirGuard
{
public:
    TempDirGuard()
    {
        std::random_device rd;
        std::mt19937_64 gen(rd());
        std::uniform_int_distribution<std::uint64_t> dist;
        path_ = fs::temp_directory_path() / ("coreverse_renderer_vfs_shader_test_" + std::to_string(dist(gen)));
        fs::create_directories(path_);
    }

    ~TempDirGuard()
    {
        std::error_code ec;
        fs::remove_all(path_, ec);
    }

    TempDirGuard(const TempDirGuard&) = delete;
    TempDirGuard& operator=(const TempDirGuard&) = delete;

    const fs::path& path() const
    {
        return path_;
    }

private:
    fs::path path_;
};

bool Fail(const std::string& what)
{
    std::fprintf(stderr, "FAIL: %s\n", what.c_str());
    return false;
}

bool WriteFile(const fs::path& path, const std::vector<std::byte>& bytes)
{
    std::error_code ec;
    fs::create_directories(path.parent_path(), ec);
    std::ofstream out(path, std::ios::binary | std::ios::trunc);
    out.write(reinterpret_cast<const char*>(bytes.data()), static_cast<std::streamsize>(bytes.size()));
    return out.good();
}

std::vector<std::byte> BuiltinBytes()
{
    const auto words = renderer::ColoredMeshShader();
    std::vector<std::byte> bytes(words.size_bytes());
    std::memcpy(bytes.data(), words.data(), bytes.size());
    return bytes;
}

bool Run(renderer::RenderDevice& device, const fs::path& projectRoot)
{
    const std::vector<std::byte> good = BuiltinBytes();
    const renderer::ShaderLoader loader(device, renderer_vfs::MakeVfsShaderReader());

    // --- 1. VFS not initialized yet: fallback with a reason, no failure ---
    {
        auto shader = loader.Load(renderer::kColoredMeshShaderName);
        if (!shader) {
            return Fail(
                "loader failed instead of falling back while the VFS is uninitialized: " + shader.error().detail
            );
        }
        if (shader->origin != renderer::ShaderOrigin::Builtin || shader->fallbackReason.empty() ||
            shader->fallbackReason.find("shaders/colored_mesh.spv") == std::string::npos) {
            return Fail(
                "uninitialized VFS: expected the built-in shader with a reason naming the file, got '" +
                shader->fallbackReason + "'"
            );
        }
        std::printf("  uninitialized VFS -> built-in, reason: %s\n", shader->fallbackReason.c_str());
    }

    if (auto init = cv_ffi::vfs::Init(projectRoot.string(), cv_ffi::vfs::Mode::Development); !init) {
        return Fail("cv_ffi::vfs::Init failed: " + init.error());
    }
    const fs::path shaders = projectRoot / "Assets" / "shaders"; // Root::Assets in Development mode

    // --- 2. initialized, but no file: built-in again ---
    {
        auto shader = loader.Load(renderer::kColoredMeshShaderName);
        if (!shader || shader->origin != renderer::ShaderOrigin::Builtin || shader->fallbackReason.empty()) {
            return Fail("missing file: expected the built-in shader with a reason");
        }
        std::printf("  missing file      -> built-in, reason: %s\n", shader->fallbackReason.c_str());
    }

    // --- 3. the file in Assets/shaders is found and wins ---
    if (!WriteFile(shaders / "colored_mesh.spv", good)) {
        return Fail("could not write the test shader file");
    }
    {
        auto shader = loader.Load(renderer::kColoredMeshShaderName);
        if (!shader) {
            return Fail("loader failed on a good file in Assets/shaders: " + shader.error().detail);
        }
        if (shader->origin != renderer::ShaderOrigin::Loaded || !shader->fallbackReason.empty() ||
            !shader->module.IsValid()) {
            return Fail(
                "the file in Assets/shaders was not used (origin=Builtin, reason='" + shader->fallbackReason + "')"
            );
        }
        // What the reader hands out is byte-for-byte the generator's output.
        auto raw = cv_ffi::vfs::ReadBytes(cv_ffi::vfs::Root::Assets, "shaders/colored_mesh.spv");
        if (!raw || *raw != good) {
            return Fail("ReadBytes(Assets, shaders/colored_mesh.spv) did not return the file's bytes");
        }
    }

    // --- 4. a shader that only exists as a file, and one in a sub-folder ---
    if (!WriteFile(shaders / "only_a_file.spv", good) || !WriteFile(shaders / "lit" / "pbr.spv", good)) {
        return Fail("could not write the extra test shader files");
    }
    for (const char* name : {"only_a_file", "lit/pbr"}) {
        auto shader = loader.Load(name);
        if (!shader || shader->origin != renderer::ShaderOrigin::Loaded) {
            return Fail(std::string("shader '") + name + "' was not loaded from the VFS");
        }
    }
    if (loader.Load("not_there_and_not_builtin")) {
        return Fail("a shader that is neither a file nor a built-in was found");
    }

    // --- 5. a corrupt or truncated file falls back, with the reason ---
    std::vector<std::byte> wrongMagic = good;
    wrongMagic[0] = std::byte{0x55};
    std::vector<std::byte> truncated(good.begin(), good.begin() + 22); // not a multiple of 4
    for (const auto& [what, bytes, expectedInReason] :
         {std::tuple<const char*, const std::vector<std::byte>&, const char*>{"wrong magic", wrongMagic, "magic"},
          {"truncated", truncated, "multiple of 4"}}) {
        if (!WriteFile(shaders / "colored_mesh.spv", bytes)) {
            return Fail("could not overwrite the test shader file");
        }
        auto shader = loader.Load(renderer::kColoredMeshShaderName);
        if (!shader || shader->origin != renderer::ShaderOrigin::Builtin ||
            shader->fallbackReason.find(expectedInReason) == std::string::npos) {
            return Fail(
                std::string(what) + ": expected a fallback to the built-in shader with '" + expectedInReason +
                "' in the reason"
            );
        }
        std::printf("  %-17s -> built-in, reason: %s\n", what, shader->fallbackReason.c_str());
    }

    // --- 6. restoring the file makes it win again (no stale cache) ---
    if (!WriteFile(shaders / "colored_mesh.spv", good)) {
        return Fail("could not restore the test shader file");
    }
    auto again = loader.Load(renderer::kColoredMeshShaderName);
    if (!again || again->origin != renderer::ShaderOrigin::Loaded) {
        return Fail("the restored file was not picked up");
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

    const TempDirGuard project;
    const bool ok = Run(*device, project.path());
    device->Shutdown();
    if (!ok) {
        return 1;
    }
    std::printf("renderer vfs shader loading OK\n");
    return 0;
}
