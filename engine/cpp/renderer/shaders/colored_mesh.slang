// Built-in "colored mesh" shader: position + per-vertex color, transformed by one matrix from
// the push constant block (Mat4 from cv-math). Vertex and fragment stage live in one Slang
// module; shaders/generate_builtin_shaders.sh compiles each entry point to its own SPIR-V blob
// and embeds them in src/BuiltinShaders.cpp.
//
// Matrices are compiled column-major (-matrix-layout-column-major), the layout cv-math uses,
// so `mul(pc.mvp, v)` applies the matrix to a column vector exactly like GLSL's `mvp * v`.

struct PushConstants
{
    float4x4 mvp;
};

[[vk::push_constant]] ConstantBuffer<PushConstants> pc;

struct VertexInput
{
    [[vk::location(0)]] float3 position : POSITION;
    [[vk::location(1)]] float3 color : COLOR;
};

struct VertexOutput
{
    float4 position : SV_Position;
    [[vk::location(0)]] float3 color : COLOR;
};

struct FragmentOutput
{
    [[vk::location(0)]] float4 color : SV_Target;
};

[shader("vertex")]
VertexOutput vertexMain(VertexInput input)
{
    VertexOutput output;
    output.position = mul(pc.mvp, float4(input.position, 1.0));
    output.color = input.color;
    return output;
}

[shader("fragment")]
FragmentOutput fragmentMain(VertexOutput input)
{
    FragmentOutput output;
    output.color = float4(input.color, 1.0);
    return output;
}
