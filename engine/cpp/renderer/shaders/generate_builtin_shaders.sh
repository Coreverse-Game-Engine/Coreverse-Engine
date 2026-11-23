#!/usr/bin/env bash
# Regenerates src/BuiltinShaders.cpp (SPIR-V of the built-in shaders) from the Slang sources in
# this directory. Thin wrapper around generate_builtin_shaders.py (which also runs on Windows:
# `py engine\cpp\renderer\shaders\generate_builtin_shaders.py --download`). The generated file
# is committed on purpose: building the renderer must not require a shader compiler.
#
# Arguments are passed through (--download, --check, --slangc PATH, --reflection DIR).
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec python3 "$here/generate_builtin_shaders.py" "$@"
