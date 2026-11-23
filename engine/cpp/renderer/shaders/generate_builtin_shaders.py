#!/usr/bin/env python3
"""Regenerates src/BuiltinShaders.cpp (the SPIR-V of the engine's built-in shaders) from the
Slang sources in this directory. The generated file is committed on purpose: building the
renderer must not require a shader compiler.

Needs the pinned Slang compiler (slang-version.json) and python3. slangc is looked up in this
order: --slangc, $SLANGC, <repo>/.tools/slang/<version>/ (see devkit/scripts/get_slangc.py),
PATH. Pass --download to fetch the pinned build when it is not found. A slangc whose version
differs from the pin is rejected, because other versions emit different SPIR-V.

    python3 engine/cpp/renderer/shaders/generate_builtin_shaders.py --download
    python3 engine/cpp/renderer/shaders/generate_builtin_shaders.py --check      # CI

--check regenerates into a temporary file and fails if src/BuiltinShaders.cpp differs.
--reflection DIR additionally writes Slang's reflection JSON next to DIR (not committed yet;
it becomes the input of the descriptor layouts later).
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

# (module file, stage, entry point, C++ array name)
SHADERS = [
    ("colored_mesh.slang", "vertex", "vertexMain", "kColoredMeshVert"),
    ("colored_mesh.slang", "fragment", "fragmentMain", "kColoredMeshFrag"),
]

# -matrix-layout-column-major: cv-math matrices are column-major; with this flag `mul(M, v)`
# in Slang equals GLSL's `M * v` on the same memory (see the comment in colored_mesh.slang).
COMMON_FLAGS = ["-target", "spirv", "-profile", "spirv_1_3", "-matrix-layout-column-major"]


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


def compile_stage(slangc: str, source: Path, stage: str, entry: str, out: Path) -> None:
    cmd = [slangc, str(source), *COMMON_FLAGS, "-entry", entry, "-stage", stage, "-o", str(out)]
    result = subprocess.run(cmd, capture_output=True, text=True, check=False)
    if result.returncode != 0 or result.stderr.strip():
        sys.exit(f"error: slangc failed for {source.name}:{entry}\n{result.stdout}{result.stderr}")


def reflect(slangc: str, source: Path, out_dir: Path) -> None:
    out_dir.mkdir(parents=True, exist_ok=True)
    cmd = [slangc, str(source), *COMMON_FLAGS]
    for _, stage, entry, _ in (s for s in SHADERS if s[0] == source.name):
        cmd += ["-entry", entry, "-stage", stage]
    with tempfile.TemporaryDirectory() as tmp:
        cmd += ["-reflection-json", str(out_dir / (source.stem + ".json")), "-o", str(Path(tmp) / "all.spv")]
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


def generate(slangc: str, out: Path) -> None:
    with tempfile.TemporaryDirectory() as tmp:
        tmp_dir = Path(tmp)
        arrays = []
        for module, stage, entry, name in SHADERS:
            spv = tmp_dir / f"{name}.spv"
            compile_stage(slangc, HERE / module, stage, entry, spv)
            validate(spv)
            arrays.append(array(name, words(spv)))

    sources = sorted({module for module, _, _, _ in SHADERS})
    with open(out, "w", encoding="utf-8", newline="\n") as f:
        f.write(f"// GENERATED by shaders/generate_builtin_shaders.py (Slang {PIN['version']}) from shaders/{', '.join(sources)} - do not edit.\n")
        f.write('#include "renderer/BuiltinShaders.hpp"\n\nnamespace renderer {\n\nnamespace {\n\n')
        f.write("\n".join(arrays))
        f.write("\n} // namespace\n\n")
        f.write("std::span<const std::uint32_t> ColoredMeshVertexShader() noexcept\n{\n    return kColoredMeshVert;\n}\n\n")
        f.write("std::span<const std::uint32_t> ColoredMeshFragmentShader() noexcept\n{\n    return kColoredMeshFrag;\n}\n\n")
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
    parser.add_argument("--reflection", metavar="DIR", help="also write Slang reflection JSON into DIR")
    args = parser.parse_args()

    slangc = find_slangc(args.slangc, args.download)

    if args.reflection:
        for module in sorted({m for m, _, _, _ in SHADERS}):
            reflect(slangc, HERE / module, Path(args.reflection))

    if args.check:
        with tempfile.TemporaryDirectory() as tmp:
            fresh = Path(tmp) / "BuiltinShaders.cpp"
            generate(slangc, fresh)
            if fresh.read_bytes() != OUT.read_bytes():
                sys.exit("error: src/BuiltinShaders.cpp is out of date. Run engine/cpp/renderer/shaders/generate_builtin_shaders.sh "
                         "(or .py) and commit the result.")
        print("src/BuiltinShaders.cpp is up to date")
        return

    generate(slangc, OUT)
    print(f"wrote {OUT}")


if __name__ == "__main__":
    main()
