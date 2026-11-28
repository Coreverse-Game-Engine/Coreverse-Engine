#pragma once

#include <string_view>

#include "cv-ffi/Vfs.hpp"
#include "renderer/ShaderLoader.hpp"

namespace renderer_vfs {

/// A renderer::ShaderBinaryReader that reads through the VFS: the path ShaderLoader asks for
/// ("shaders/<name>.spv") is taken relative to `root`.
///
/// Compiled shaders are assets the game ships, so the default is Root::Assets - the packed root:
/// loose files under <project>/Assets/shaders/ in Development mode, entries of the archive in
/// Release mode. (Root::Build would be wrong: it holds build output that is not part of a release.)
///
/// The VFS must be initialized (cv_ffi::vfs::Init) before the reader is used; until then every
/// read fails with the VFS's error, which ShaderLoader turns into a fallback to the built-in
/// shader (LoadedShader::fallbackReason) - not into a crash.
[[nodiscard]] inline renderer::ShaderBinaryReader
MakeVfsShaderReader(cv_ffi::vfs::Root root = cv_ffi::vfs::Root::Assets)
{
    return [root](std::string_view path) { return cv_ffi::vfs::ReadBytes(root, path); };
}

} // namespace renderer_vfs
