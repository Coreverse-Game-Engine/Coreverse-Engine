#pragma once

namespace renderer {

class RenderDevice;

/// Move-only RAII handle to a compiled shader module (the Vulkan backend: a VkShaderModule made
/// from SPIR-V). A module may hold several entry points - the built-in colored mesh shader keeps
/// its vertex and fragment stage in one module - which a pipeline picks by name through
/// ShaderStageDesc (GraphicsPipeline.hpp).
///
/// Created by RenderDevice::CreateShaderModule() or ShaderLoader::Load() (ShaderLoader.hpp);
/// released by the destructor. Same ordering rule as every device-owned resource: destroy it
/// before the device's Shutdown(). A pipeline copies what it needs while it is created, so a
/// module can be destroyed right after CreateGraphicsPipeline() returned.
class ShaderModule
{
public:
    ShaderModule() noexcept = default;
    ~ShaderModule();

    ShaderModule(const ShaderModule&) = delete;
    ShaderModule& operator=(const ShaderModule&) = delete;

    ShaderModule(ShaderModule&& other) noexcept;
    ShaderModule& operator=(ShaderModule&& other) noexcept;

    [[nodiscard]] bool IsValid() const noexcept
    {
        return m_nativeHandle != nullptr;
    }

    /// Backend-specific handle (a heap-allocated struct in the Vulkan backend); same contract as
    /// Buffer::GetNativeHandle().
    [[nodiscard]] void* GetNativeHandle() const noexcept
    {
        return m_nativeHandle;
    }

private:
    friend class RenderDevice;

    ShaderModule(RenderDevice* device, void* nativeHandle) noexcept : m_device(device), m_nativeHandle(nativeHandle) {}

    void Release() noexcept;

    RenderDevice* m_device = nullptr;
    void* m_nativeHandle = nullptr;
};

} // namespace renderer
