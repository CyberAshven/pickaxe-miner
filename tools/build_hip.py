#!/usr/bin/env python3
"""Build architecture-specific PHOTON HIP device code objects."""

from __future__ import annotations

import argparse
import hashlib
import os
from pathlib import Path
import shutil
import struct
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
SOURCES = (
    "stage_a_rfc6979",
    "photon_stage_b16",
    "photon_c1_schnorr",
    "stage_c_hash",
)
CLANG_OFFLOAD_BUNDLE_MAGIC = b"__CLANG_OFFLOAD_BUNDLE__"


def normalize_code_object(path: Path, arch: str) -> None:
    payload = path.read_bytes()
    if payload.startswith(b"\x7fELF"):
        return
    if not payload.startswith(CLANG_OFFLOAD_BUNDLE_MAGIC):
        raise RuntimeError(f"{path}: HIP compiler output is neither ELF nor a Clang offload bundle")

    cursor = len(CLANG_OFFLOAD_BUNDLE_MAGIC)
    if cursor + 8 > len(payload):
        raise RuntimeError(f"{path}: truncated Clang offload bundle header")
    bundle_count = struct.unpack_from("<Q", payload, cursor)[0]
    cursor += 8
    selected: bytes | None = None
    selected_target = ""
    expected_target = f"amdgcn-amd-amdhsa--{arch}"
    for _ in range(bundle_count):
        if cursor + 24 > len(payload):
            raise RuntimeError(f"{path}: truncated Clang offload bundle descriptor")
        offset, size, target_size = struct.unpack_from("<QQQ", payload, cursor)
        cursor += 24
        if cursor + target_size > len(payload):
            raise RuntimeError(f"{path}: truncated Clang offload bundle target")
        target = payload[cursor : cursor + target_size].decode("utf-8", errors="strict")
        cursor += target_size
        end = offset + size
        if end > len(payload):
            raise RuntimeError(f"{path}: Clang offload bundle entry exceeds file bounds")
        if expected_target in target:
            if selected is not None:
                raise RuntimeError(f"{path}: multiple device images match {arch}")
            selected = payload[offset:end]
            selected_target = target

    if selected is None:
        raise RuntimeError(f"{path}: Clang offload bundle has no {arch} device image")
    if not selected.startswith(b"\x7fELF"):
        raise RuntimeError(f"{path}: {selected_target} device image is not ELF")

    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_bytes(selected)
    temporary.replace(path)
    print(f"extracted raw {selected_target} ELF from Clang offload bundle")


def digest(path: Path) -> str:
    hasher = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            hasher.update(chunk)
    return hasher.hexdigest()


def find_compiler() -> tuple[str, bool]:
    explicit = os.environ.get("HIPCC")
    if explicit:
        return explicit, Path(explicit).name.lower().startswith("hipcc")
    found = shutil.which("hipcc")
    if found:
        return found, True
    hip_path = os.environ.get("HIP_PATH")
    if hip_path:
        for name, is_hipcc in (("hipcc.exe", True), ("clang++.exe", False), ("amdclang++", False)):
            candidate = Path(hip_path) / "bin" / name
            if candidate.is_file():
                return str(candidate), is_hipcc
    for name in ("amdclang++", "clang++"):
        found = shutil.which(name)
        if found:
            return found, False
    raise RuntimeError("no HIP compiler found; install the ROCm/HIP SDK or set HIPCC/HIP_PATH")


def build(compiler: str, is_hipcc: bool, arch: str, output_dir: Path) -> None:
    output_dir.mkdir(parents=True, exist_ok=True)
    build_env = os.environ.copy()
    build_env.setdefault("SOURCE_DATE_EPOCH", "0")
    for stem in SOURCES:
        source = ROOT / "hip" / f"{stem}.hip.cpp"
        output = output_dir / f"{stem}.hsaco"
        common = [
            "-O3",
            "-std=c++17",
            f"-ffile-prefix-map={ROOT}=.",
            f"-fdebug-prefix-map={ROOT}=.",
        ]
        if is_hipcc:
            command = [
                compiler,
                "--genco",
                f"--offload-arch={arch}",
                *common,
                str(source),
                "-o",
                str(output),
            ]
        else:
            command = [
                compiler,
                "-x",
                "hip",
                *common,
                "-c",
                "--offload-device-only",
                f"--offload-arch={arch}",
                str(source),
                "-o",
                str(output),
            ]
        print(" ".join(command))
        subprocess.run(command, check=True, cwd=ROOT, env=build_env)
        normalize_code_object(output, arch)


def verify(arch: str, output_dir: Path) -> None:
    command = [
        sys.executable,
        str(ROOT / "tools" / "verify_hip_artifacts.py"),
        "--arch",
        arch,
        "--directory",
        str(output_dir),
    ]
    subprocess.run(command, check=True, cwd=ROOT)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--arch", required=True, help="AMD GPU target, e.g. gfx1036")
    parser.add_argument(
        "--check-reproducible",
        action="store_true",
        help="build twice and require identical SHA-256 digests",
    )
    parser.add_argument(
        "--skip-verify",
        action="store_true",
        help="skip metadata/symbol/ABI validation (intended only for toolchain debugging)",
    )
    args = parser.parse_args()
    if not args.arch.startswith("gfx"):
        parser.error("--arch must be a gfx target such as gfx1036")
    try:
        compiler, is_hipcc = find_compiler()
    except RuntimeError as error:
        print(f"error: {error}", file=sys.stderr)
        return 2

    output_dir = ROOT / "hip" / "build" / args.arch
    try:
        build(compiler, is_hipcc, args.arch, output_dir)
        if not args.skip_verify:
            verify(args.arch, output_dir)

        if args.check_reproducible:
            first = {stem: digest(output_dir / f"{stem}.hsaco") for stem in SOURCES}
            build(compiler, is_hipcc, args.arch, output_dir)
            if not args.skip_verify:
                verify(args.arch, output_dir)
            second = {stem: digest(output_dir / f"{stem}.hsaco") for stem in SOURCES}
            if first != second:
                differences = [stem for stem in SOURCES if first[stem] != second[stem]]
                raise RuntimeError(
                    "non-reproducible HIP code objects: " + ", ".join(differences)
                )
            print("reproducibility check passed: repeated HSACO SHA-256 digests match")
    except (OSError, RuntimeError, subprocess.CalledProcessError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1

    print(f"built PHOTON HIP code objects for {args.arch} in {output_dir}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
