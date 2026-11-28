#pragma once

#include <cstdint>
#include <span>
#include <string_view>

#include "renderer/GraphicsPipeline.hpp"

namespace renderer {

/// The engine's built-in "colored mesh" shader: ONE SPIR-V module (compiled from colored_mesh.slang;
/// the Slang source and the generator script are in engine/cpp/renderer/shaders/) holding a vertex
/// entry point (kColoredMeshVertexEntry) and a fragment entry point (kColoredMeshFragmentEntry).
/// Vertex layout is ColoredVertex, the push constant block is one 4x4 matrix (the MVP, 64 bytes,
/// column-major like cv_math::Mat4). Normally obtained as a ShaderModule through
/// ShaderLoader::Load(kColoredMeshShaderName), which prefers a colored_mesh.spv from the VFS.
[[nodiscard]] std::span<const std::uint32_t> ColoredMeshShader() noexcept;

inline constexpr std::string_view kColoredMeshShaderName = "colored_mesh";
inline constexpr std::string_view kColoredMeshVertexEntry = "vertexMain";
inline constexpr std::string_view kColoredMeshFragmentEntry = "fragmentMain";

/// The built-in shader modules by logical name (the name ShaderLoader::Load() takes, without
/// directory or extension); an empty span if there is none of that name.
[[nodiscard]] std::span<const std::uint32_t> FindBuiltinShader(std::string_view name) noexcept;

/// Vertex for the colored mesh shaders: location 0 = position, location 1 = color.
struct ColoredVertex {
    float position[3];
    float color[3];
};
static_assert(sizeof(ColoredVertex) == 24, "ColoredVertex must be tightly packed");

/// The two attributes of ColoredVertex, for GraphicsPipelineDesc::vertexAttributes.
inline constexpr VertexAttribute kColoredVertexAttributes[] = {
    {0, VertexFormat::Float3, 0},
    {1, VertexFormat::Float3, 12},
};

/// Push constant size the colored mesh shaders expect (one mat4).
inline constexpr std::uint32_t kColoredMeshPushConstantSize = 64;

} // namespace renderer
