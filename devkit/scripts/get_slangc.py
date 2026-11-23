#!/usr/bin/env python3
"""Downloads the pinned Slang compiler (slangc) into <repo>/.tools/slang/<version>/.

The pin (version, per-platform archive, SHA-256) lives in
engine/cpp/renderer/shaders/slang-version.json. Building the engine does NOT need slangc
(the SPIR-V of the built-in shaders is committed); it is only needed to regenerate
src/BuiltinShaders.cpp with engine/cpp/renderer/shaders/generate_builtin_shaders.py.

Prints the path of slangc on stdout (everything else goes to stderr), so it can be used as

    SLANGC="$(python3 devkit/scripts/get_slangc.py)"

Works on Windows, Linux and macOS; needs only the Python standard library.
"""

from __future__ import annotations

import hashlib
import json
import os
import platform
import shutil
import sys
import tarfile
import tempfile
import urllib.request
import zipfile
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
PIN_FILE = REPO / "engine" / "cpp" / "renderer" / "shaders" / "slang-version.json"
TOOLS_DIR = REPO / ".tools" / "slang"


def load_pin() -> dict:
    return json.loads(PIN_FILE.read_text(encoding="utf-8"))


def host_platform() -> str:
    system = {"Windows": "windows", "Linux": "linux", "Darwin": "macos"}.get(platform.system())
    machine = platform.machine().lower()
    arch = {"amd64": "x86_64", "x86_64": "x86_64", "arm64": "aarch64", "aarch64": "aarch64"}.get(machine)
    if system is None or arch is None:
        sys.exit(f"get_slangc: unsupported platform {platform.system()} / {platform.machine()}")
    return f"{system}-{arch}"


def slangc_path(version: str) -> Path:
    exe = "slangc.exe" if platform.system() == "Windows" else "slangc"
    return TOOLS_DIR / version / "bin" / exe


def _safe_members(names: list[str], dest: Path) -> None:
    root = dest.resolve()
    for name in names:
        target = (dest / name).resolve()
        if root != target and root not in target.parents:
            sys.exit(f"get_slangc: archive entry escapes the target directory: {name}")


def extract(archive: Path, dest: Path) -> None:
    dest.mkdir(parents=True, exist_ok=True)
    if archive.suffix == ".zip":
        with zipfile.ZipFile(archive) as zf:
            _safe_members(zf.namelist(), dest)
            zf.extractall(dest)
    else:
        with tarfile.open(archive, "r:gz") as tf:
            _safe_members(tf.getnames(), dest)
            # The Linux archive ships library symlinks (libfoo.so -> libfoo.so.1); allow them
            # as long as they point inside the extraction directory.
            for member in tf.getmembers():
                if member.issym():
                    link_target = (dest / member.name).parent / member.linkname
                    _safe_members([os.path.relpath(link_target, dest)], dest)
                elif member.islnk():
                    _safe_members([member.linkname], dest)
                elif not (member.isfile() or member.isdir()):
                    sys.exit(f"get_slangc: unexpected entry type in archive: {member.name}")
            tf.extractall(dest)


def ensure_slangc() -> Path:
    pin = load_pin()
    version = pin["version"]
    exe = slangc_path(version)
    if exe.is_file():
        return exe

    plat = host_platform()
    entry = pin["platforms"].get(plat)
    if entry is None:
        sys.exit(f"get_slangc: no pinned Slang build for {plat} in {PIN_FILE}")

    url = pin["url"].format(version=version, platform=plat, ext=entry["ext"])
    print(f"get_slangc: downloading {url}", file=sys.stderr)

    with tempfile.TemporaryDirectory() as tmp:
        archive = Path(tmp) / f"slang.{entry['ext']}"
        digest = hashlib.sha256()
        with urllib.request.urlopen(url, timeout=120) as response, open(archive, "wb") as out:
            while chunk := response.read(1 << 20):
                digest.update(chunk)
                out.write(chunk)
        if digest.hexdigest() != entry["sha256"]:
            sys.exit(f"get_slangc: SHA-256 mismatch for {url}\n  expected {entry['sha256']}\n  got      {digest.hexdigest()}")

        staging = Path(tmp) / "out"
        extract(archive, staging)

        target = TOOLS_DIR / version
        if target.exists():
            shutil.rmtree(target)
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.move(str(staging), str(target))

    if platform.system() != "Windows":
        for binary in (TOOLS_DIR / version / "bin").iterdir():
            if binary.is_file() and binary.suffix == "" and not binary.name.endswith(".slang"):
                binary.chmod(binary.stat().st_mode | 0o111)

    if not exe.is_file():
        sys.exit(f"get_slangc: {exe} is missing after extraction")
    print(f"get_slangc: installed {exe}", file=sys.stderr)
    return exe


if __name__ == "__main__":
    print(ensure_slangc())
