# renderer-vfs: the glue between the renderer and the VFS - a ShaderBinaryReader that reads shader
# files through cv_ffi::vfs (see renderer/ShaderLoader.hpp for why the renderer itself does not
# link `ffi`). Header-only; link it from whatever hosts both the renderer and the VFS (editor,
# runtime, tests).

add_library(renderer-vfs INTERFACE)
add_library(renderer::vfs ALIAS renderer-vfs)

target_include_directories(renderer-vfs INTERFACE ${CMAKE_CURRENT_SOURCE_DIR}/include)
target_link_libraries(renderer-vfs INTERFACE renderer::renderer cv-ffi::cv-ffi)
