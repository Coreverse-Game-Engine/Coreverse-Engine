#!/usr/bin/env python3
"""Regenerates src/BuiltinShaders.cpp (the SPIR-V of the engine's built-in shaders) from the
Slang sources in this directory. The generated file is committed on purpose: building the
renderer must not require a shader compiler.

Each Slang module becomes ONE SPIR-V module holding all of its entry points (compiled with
-fvk-use-entrypoint-name, so the entry points keep their Slang names: vertexMain, fragmentMain).
The renderer picks them by name (GraphicsPipelineDesc::vertex/fragment, see BuiltinShaders.hpp).

Needs the pinned Slang compiler (slang-version.json) and python3. slangc is looked up in this
order: --slangc, $SLANGC, <repo>/.tools/slang/<version>/ (see devkit/scripts/get_slangc.py),
PATH. Pass --download to fetch the pinned build when it is not found. A slangc whose version
differs from the pin is rejected, because other versions emit different SPIR-V.

    python3 engine/cpp/renderer/shaders/generate_builtin_shaders.py --download
    python3 engine/cpp/renderer/shaders/generate_builtin_shaders.py --check      # CI

--check regenerates into a temporary file and fails if src/BuiltinShaders.cpp differs.
--spv-dir DIR additionally writes each module as DIR/<name>.spv - the file ShaderLoader reads
from the VFS as `shaders/<name>.spv` (Assets root) in preference to the built-in copy.
--reflection DIR additionally writes Slang's reflection JSON into DIR (not committed yet; it
becomes the input of the descriptor layouts later).
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import struct
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[3]
OUT = HERE.parent / "src" / "BuiltinShaders.cpp"
PIN = json.loads((HERE / "slang-version.json").read_text(encoding="utf-8"))

# One entry per Slang module: logical shader name (= ShaderLoader name and .spv file name),
# source file, entry points (name, stage), C++ array name, and the BuiltinShaders.hpp constants
# that must spell the entry point names (checked by a static_assert in the generated file).
MODULES = [
    {
        "name": "colored_mesh",
        "source": "colored_mesh.slang",
        "entries": [("vertexMain", "vertex"), ("fragmentMain", "fragment")],
        "array": "kColoredMesh",
        "accessor": "ColoredMeshShader",
        "name_constant": "kColoredMeshShaderName",
        "entry_constants": {"kColoredMeshVertexEntry": "vertexMain", "kColoredMeshFragmentEntry": "fragmentMain"},
    },
]

# -matrix-layout-column-major: cv-math matrices are column-major; with this flag `mul(M, v)`
# in Slang equals GLSL's `M * v` on the same memory (see the comment in colored_mesh.slang).
COMMON_FLAGS = ["-target", "spirv", "-profile", "spirv_1_3", "-matrix-layout-column-major", "-fvk-use-entrypoint-name"]


def exe_name() -> str:
    return "slangc.exe" if os.name == "nt" else "slangc"


def slangc_version(path: str) -> str:
    # slangc prints its version on stderr (older builds: stdout), so look at both.
    result = subprocess.run([path, "-version"], capture_output=True, text=True, check=False)
    text = (result.stdout + result.stderr).strip()
    return text.splitlines()[0].strip() if text else ""


def find_slangc(explicit: str | None, download: bool) -> str:
    candidates = [explicit, os.environ.get("SLANGC")]
    pinned = REPO / ".tools" / "slang" / PIN["version"] / "bin" / exe_name()
    candidates.append(str(pinned) if pinned.is_file() else None)
    candidates.append(shutil.which("slangc"))

    for candidate in candidates:
        if not candidate:
            continue
        version = slangc_version(candidate)
        if version == PIN["version"]:
            return candidate
        sys.exit(f"error: {candidate} is Slang {version or '(unknown)'}, but this repository pins {PIN['version']} "
                 f"(shaders/slang-version.json). Run devkit/scripts/get_slangc.py or pass --slangc.")

    if download:
        sys.path.insert(0, str(REPO / "devkit" / "scripts"))
        import get_slangc  # noqa: PLC0415

        return str(get_slangc.ensure_slangc())
    sys.exit(f"error: slangc {PIN['version']} not found. Re-run with --download, or run devkit/scripts/get_slangc.py.")


def entry_args(module: dict) -> list[str]:
    args: list[str] = []
    for entry, stage in module["entries"]:
        args += ["-entry", entry, "-stage", stage]
    return args


def compile_module(slangc: str, module: dict, out: Path) -> None:
    source = HERE / module["source"]
    cmd = [slangc, str(source), *COMMON_FLAGS, *entry_args(module), "-o", str(out)]
    result = subprocess.run(cmd, capture_output=True, text=True, check=False)
    if result.returncode != 0 or result.stderr.strip():
        sys.exit(f"error: slangc failed for {source.name}\n{result.stdout}{result.stderr}")


def reflect(slangc: str, module: dict, out_dir: Path) -> None:
    out_dir.mkdir(parents=True, exist_ok=True)
    source = HERE / module["source"]
    with tempfile.TemporaryDirectory() as tmp:
        cmd = [slangc, str(source), *COMMON_FLAGS, *entry_args(module),
               "-reflection-json", str(out_dir / (source.stem + ".json")), "-o", str(Path(tmp) / "all.spv")]
        result = subprocess.run(cmd, capture_output=True, text=True, check=False)
        if result.returncode != 0:
            sys.exit(f"error: slangc reflection failed for {source.name}\n{result.stdout}{result.stderr}")


def words(path: Path) -> tuple[int, ...]:
    data = path.read_bytes()
    assert len(data) % 4 == 0 and struct.unpack("<I", data[:4])[0] == 0x07230203, "not a SPIR-V binary"
    return struct.unpack("<%dI" % (len(data) // 4), data)


def array(name: str, ws: tuple[int, ...]) -> str:
    lines = ["    " + ", ".join("0x%08x" % w for w in ws[i:i + 8]) + "," for i in range(0, len(ws), 8)]
    return "constexpr std::uint32_t %s[] = {\n%s\n};\n" % (name, "\n".join(lines))


def validate(spv: Path) -> None:
    tool = shutil.which("spirv-val")
    if tool is None:
        return
    result = subprocess.run([tool, "--target-env", "vulkan1.3", str(spv)], capture_output=True, text=True, check=False)
    if result.returncode != 0:
        sys.exit(f"error: spirv-val rejected {spv.name}:\n{result.stdout}{result.stderr}")


def generate(slangc: str, out: Path, spv_dir: Path | None = None) -> None:
    arrays = []
    with tempfile.TemporaryDirectory() as tmp:
        tmp_dir = Path(tmp)
        for module in MODULES:
            spv = tmp_dir / f"{module['name']}.spv"
            compile_module(slangc, module, spv)
            validate(spv)
            arrays.append(array(module["array"], words(spv)))
            if spv_dir is not None:
                spv_dir.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(spv, spv_dir / spv.name)

    sources = ", ".join(sorted({m["source"] for m in MODULES}))
    with open(out, "w", encoding="utf-8", newline="\n") as f:
        f.write(f"// GENERATED by shaders/generate_builtin_shaders.py (Slang {PIN['version']}) from shaders/{sources} - do not edit.\n")
        f.write('#include "renderer/BuiltinShaders.hpp"\n\nnamespace renderer {\n\nnamespace {\n\n')
        f.write("\n".join(arrays))
        f.write("\n")
        for module in MODULES:
            for constant, entry in module["entry_constants"].items():
                f.write(f'static_assert({constant} == "{entry}", "BuiltinShaders.hpp and {module["source"]} disagree on an entry point name");\n')
            f.write(f'static_assert({module["name_constant"]} == "{module["name"]}");\n')
        f.write("\n} // namespace\n\n")
        for module in MODULES:
            f.write(f"std::span<const std::uint32_t> {module['accessor']}() noexcept\n{{\n    return {module['array']};\n}}\n\n")
        f.write("std::span<const std::uint32_t> FindBuiltinShader(std::string_view name) noexcept\n{\n")
        for module in MODULES:
            f.write(f"    if (name == {module['name_constant']}) {{\n        return {module['array']};\n    }}\n")
        f.write("    return {};\n}\n\n")
        f.write("} // namespace renderer\n")

    # The CI formatting check covers this file too, so hand it to clang-format when available.
    clang_format = shutil.which("clang-format")
    if clang_format:
        # Through stdin: --assume-filename only applies there, and it is what makes clang-format
        # find the repository's .clang-format even when `out` is a temporary file (--check).
        formatted = subprocess.run([clang_format, "--style=file", f"--assume-filename={OUT}"],
                                   input=out.read_bytes(), capture_output=True, check=True).stdout
        out.write_bytes(formatted)
    else:
        print("warning: clang-format not found; run devkit/scripts/format.sh before committing", file=sys.stderr)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--slangc", help="path to slangc (must be the pinned version)")
    parser.add_argument("--download", action="store_true", help="download the pinned slangc when it is not found")
    parser.add_argument("--check", action="store_true", help="fail if src/BuiltinShaders.cpp is out of date")
    parser.add_argument("--spv-dir", metavar="DIR", help="also write each module as DIR/<name>.spv (the VFS shader file)")
    parser.add_argument("--reflection", metavar="DIR", help="also write Slang reflection JSON into DIR")
    args = parser.parse_args()

    slangc = find_slangc(args.slangc, args.download)

    if args.reflection:
        for module in MODULES:
            reflect(slangc, module, Path(args.reflection))

    if args.check:
        with tempfile.TemporaryDirectory() as tmp:
            fresh = Path(tmp) / "BuiltinShaders.cpp"
            generate(slangc, fresh)  # --spv-dir is not written in check mode: it is not committed
            if fresh.read_bytes() != OUT.read_bytes():
                sys.exit("error: src/BuiltinShaders.cpp is out of date. Run engine/cpp/renderer/shaders/generate_builtin_shaders.sh "
                         "(or .py) and commit the result.")
        print("src/BuiltinShaders.cpp is up to date")
        return

    generate(slangc, OUT, Path(args.spv_dir) if args.spv_dir else None)
    print(f"wrote {OUT}")
    if args.spv_dir:
        print(f"wrote {', '.join(m['name'] + '.spv' for m in MODULES)} to {args.spv_dir}")


if __name__ == "__main__":
    main()
