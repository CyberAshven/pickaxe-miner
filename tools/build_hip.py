#!/usr/bin/env python3
"""Build architecture-specific PHOTON HIP device code objects."""

from __future__ import annotations

import argparse
import os
from pathlib import Path
import shutil
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
SOURCES = (
    "stage_a_rfc6979",
    "photon_stage_b16",
    "photon_c1_schnorr",
    "stage_c_hash",
)


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


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--arch", required=True, help="AMD GPU target, e.g. gfx1036")
    args = parser.parse_args()
    if not args.arch.startswith("gfx"):
        parser.error("--arch must be a gfx target such as gfx1036")
    try:
        compiler, is_hipcc = find_compiler()
    except RuntimeError as error:
        print(f"error: {error}", file=sys.stderr)
        return 2
    output_dir = ROOT / "hip" / "build" / args.arch
    output_dir.mkdir(parents=True, exist_ok=True)
    for stem in SOURCES:
        source = ROOT / "hip" / f"{stem}.hip.cpp"
        output = output_dir / f"{stem}.hsaco"
        if is_hipcc:
            command = [compiler, "--genco", f"--offload-arch={args.arch}", "-O3", "-std=c++17", str(source), "-o", str(output)]
        else:
            command = [compiler, "-x", "hip", "-O3", "-std=c++17", "-c", "--offload-device-only", f"--offload-arch={args.arch}", str(source), "-o", str(output)]
        print(" ".join(command))
        subprocess.run(command, check=True, cwd=ROOT)
    print(f"built PHOTON HIP code objects for {args.arch} in {output_dir}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
