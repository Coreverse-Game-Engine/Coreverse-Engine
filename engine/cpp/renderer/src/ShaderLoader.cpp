#include "renderer/ShaderLoader.hpp"

#include <algorithm>
#include <cstdint>
#include <cstring>
#include <utility>

#include "renderer/BuiltinShaders.hpp"
#include "renderer/RenderDevice.hpp"

namespace renderer {

namespace {

constexpr std::uint32_t kSpirvMagic = 0x07230203;
constexpr std::size_t kMinSpirvBytes =
    5 * sizeof(std::uint32_t); // the header: magic, version, generator, bound, schema

bool IsValidName(std::string_view name) noexcept
{
    if (name.empty() || name.front() == '/' || name.back() == '/') {
        return false;
    }
    if (name.find_first_of("\\:") != std::string_view::npos) {
        return false;
    }
    // No empty, "." or ".." path segment.
    std::size_t begin = 0;
    while (begin <= name.size()) {
        const std::size_t end = std::min(name.find('/', begin), name.size());
        const std::string_view segment = name.substr(begin, end - begin);
        if (segment.empty() || segment == "." || segment == "..") {
            return false;
        }
        begin = end + 1;
    }
    return true;
}

/// SPIR-V is a stream of 32-bit words in the producer's byte order; every platform the engine
/// targets is little-endian, which is also what slangc/glslang write, so the words are copied as
/// they are and a file of the other byte order fails the magic number check.
std::expected<std::vector<std::uint32_t>, std::string> ToSpirvWords(const std::vector<std::byte>& bytes)
{
    if (bytes.size() < kMinSpirvBytes) {
        return std::unexpected("too small to be a SPIR-V binary (" + std::to_string(bytes.size()) + " bytes)");
    }
    if (bytes.size() % sizeof(std::uint32_t) != 0) {
        return std::unexpected("size (" + std::to_string(bytes.size()) + " bytes) is not a multiple of 4");
    }
    std::vector<std::uint32_t> words(bytes.size() / sizeof(std::uint32_t));
    std::memcpy(words.data(), bytes.data(), bytes.size());
    if (words[0] != kSpirvMagic) {
        return std::unexpected("no SPIR-V magic number (not a SPIR-V binary, or the wrong byte order)");
    }
    return words;
}

std::unexpected<RenderError> Error(std::string detail)
{
    return std::unexpected(RenderError{RenderErrorCode::InitializationFailed, std::move(detail)});
}

} // namespace

ShaderLoader::ShaderLoader(RenderDevice& device, ShaderBinaryReader reader)
    : m_device(&device), m_reader(std::move(reader))
{}

std::string ShaderLoader::PathFor(std::string_view name)
{
    std::string path;
    path.reserve(kDirectory.size() + name.size() + kExtension.size());
    path.append(kDirectory).append(name).append(kExtension);
    return path;
}

std::expected<LoadedShader, RenderError> ShaderLoader::Load(std::string_view name) const
{
    if (!IsValidName(name)) {
        return Error("invalid shader name '" + std::string(name) + "'");
    }

    std::string reason;
    if (m_reader) {
        const std::string path = PathFor(name);
        auto bytes = m_reader(path);
        if (!bytes) {
            reason = path + ": " + bytes.error();
        } else if (auto words = ToSpirvWords(*bytes); !words) {
            reason = path + ": " + words.error();
        } else if (auto module = m_device->CreateShaderModule(*words); !module) {
            reason = path + ": " + module.error().detail;
        } else {
            return LoadedShader{std::move(*module), ShaderOrigin::Loaded, {}};
        }
    }

    const auto builtin = FindBuiltinShader(name);
    if (builtin.empty()) {
        return Error(
            "shader '" + std::string(name) + "' not found" + (reason.empty() ? "" : " (" + reason + ")") +
            ", and there is no built-in shader of that name"
        );
    }
    auto module = m_device->CreateShaderModule(builtin);
    if (!module) {
        return std::unexpected(std::move(module.error()));
    }
    return LoadedShader{std::move(*module), ShaderOrigin::Builtin, std::move(reason)};
}

} // namespace renderer
