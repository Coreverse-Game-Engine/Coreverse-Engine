#include "renderer/ShaderModule.hpp"

#include "renderer/RenderDevice.hpp"

namespace renderer {

ShaderModule::~ShaderModule()
{
    Release();
}

ShaderModule::ShaderModule(ShaderModule&& other) noexcept
    : m_device(other.m_device), m_nativeHandle(other.m_nativeHandle)
{
    other.m_device = nullptr;
    other.m_nativeHandle = nullptr;
}

ShaderModule& ShaderModule::operator=(ShaderModule&& other) noexcept
{
    if (this != &other) {
        Release();
        m_device = other.m_device;
        m_nativeHandle = other.m_nativeHandle;
        other.m_device = nullptr;
        other.m_nativeHandle = nullptr;
    }
    return *this;
}

void ShaderModule::Release() noexcept
{
    if (m_nativeHandle != nullptr && m_device != nullptr) {
        m_device->ReleaseShaderModule(m_nativeHandle);
    }
    m_nativeHandle = nullptr;
    m_device = nullptr;
}

} // namespace renderer
