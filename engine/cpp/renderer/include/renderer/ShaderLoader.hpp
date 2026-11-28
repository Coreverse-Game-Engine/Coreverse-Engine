#pragma once

#include <cstddef>
#include <expected>
#include <functional>
#include <string>
#include <string_view>
#include <vector>

#include "renderer/RenderError.hpp"
#include "renderer/ShaderModule.hpp"

namespace renderer {

class RenderDevice;

/// Reads the bytes of a shader file by logical path (e.g. "shaders/colored_mesh.spv"); the error
/// string says why it could not. Supplied by the host, on purpose: renderer is a standalone
/// library without a link to `ffi`/the Rust VFS (see RenderDevice::SetLogCallback for the same
/// reasoning), so the editor/runtime hand in a reader that goes through the VFS -
/// renderer_vfs::MakeVfsShaderReader() in engine/cpp/renderer-vfs/ - and a test can hand in one
/// that reads from a temporary directory or from memory.
using ShaderBinaryReader = std::function<std::expected<std::vector<std::byte>, std::string>(std::string_view path)>;

/// Where a loaded shader module came from.
enum class ShaderOrigin {
    Loaded,  ///< From a file read through the ShaderBinaryReader.
    Builtin, ///< From the SPIR-V compiled into the renderer (BuiltinShaders.hpp).
};

struct LoadedShader {
    ShaderModule module;
    ShaderOrigin origin = ShaderOrigin::Builtin;
    /// Empty unless a reader was configured and its file could not be used (missing, unreadable,
    /// not SPIR-V, rejected by the device): then the built-in module was used instead and this says
    /// why, so the host can log it. A loader without a reader never sets it.
    std::string fallbackReason;
};

/// Turns a shader name into a ShaderModule: first the file `shaders/<name>.spv` through the
/// reader, then - when there is no reader, or that file cannot be used - the built-in module of
/// the same name. A name that is neither a usable file nor a built-in is an error.
///
/// The loader keeps no cache and does not own the modules it returns; it only needs the device to
/// outlive it. The reader is called on the calling thread.
class ShaderLoader
{
public:
    static constexpr std::string_view kDirectory = "shaders/";
    static constexpr std::string_view kExtension = ".spv";

    /// `reader` may be empty: only built-in shaders are available then.
    explicit ShaderLoader(RenderDevice& device, ShaderBinaryReader reader = {});

    /// `name` is a logical shader name without directory or extension ("colored_mesh"); '/' may be
    /// used to address a subfolder. Empty names, absolute paths, "..", ':' and '\\' are rejected.
    [[nodiscard]] std::expected<LoadedShader, RenderError> Load(std::string_view name) const;

    /// The path the reader is asked for: "shaders/<name>.spv".
    [[nodiscard]] static std::string PathFor(std::string_view name);

private:
    RenderDevice* m_device;
    ShaderBinaryReader m_reader;
};

} // namespace renderer
