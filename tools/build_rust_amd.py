#!/usr/bin/env python3
"""Build the shared Rust GPU engine as a native HIP code object for one AMD target."""

from __future__ import annotations

import argparse
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
# The same pinned compiler builds photon_rust.ptx for CUDA.
TOOLCHAIN = "nightly-2026-04-02"
OUTPUT_NAME = "photon_rust.hsaco"
CONTRACT = ROOT / "hip" / "rust_kernel_contract.json"


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--arch", required=True, help="AMD GPU target, e.g. gfx1100")
    parser.add_argument("--skip-verify", action="store_true")
    args = parser.parse_args()
    if re.fullmatch(r"gfx[0-9a-f]{3,}", args.arch) is None:
        parser.error("--arch must be a gfx target such as gfx1100")

    target_dir = ROOT / "artifacts" / "rust-engine" / f"amdgcn-{args.arch}"
    env = os.environ.copy()
    env["CARGO_TARGET_DIR"] = str(target_dir)
    # Code-object v5 loads on AMD's Windows HIP runtime and on ROCm. The AMD
    # target always links with LTO, which needs bitcode embedded in core.
    env["RUSTFLAGS"] = (
        f"-C target-cpu={args.arch} -C embed-bitcode=yes "
        "-C llvm-args=--amdhsa-code-object-version=5 -C panic=abort"
    )
    subprocess.run(
        [
            "cargo",
            f"+{TOOLCHAIN}",
            "rustc",
            "--locked",
            "--release",
            "--features",
            "upstream-rust",
            "--manifest-path",
            str(ROOT / "rust-engine" / "Cargo.toml"),
            "--target",
            "amdgcn-amd-amdhsa",
            "--crate-type",
            "cdylib",
            "-Z",
            "build-std=core",
        ],
        check=True,
        cwd=ROOT,
        env=env,
    )

    built = target_dir / "amdgcn-amd-amdhsa" / "release" / "pickaxe_rust_engine.elf"
    output_dir = ROOT / "hip" / "build" / args.arch
    output_dir.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(built, output_dir / OUTPUT_NAME)
    if not args.skip_verify:
        subprocess.run(
            [
                sys.executable,
                str(ROOT / "tools" / "verify_hip_artifacts.py"),
                "--arch",
                args.arch,
                "--directory",
                str(output_dir),
                "--contract",
                str(CONTRACT),
            ],
            check=True,
            cwd=ROOT,
        )
    print(f"built shared Rust HIP code object for {args.arch}: {output_dir / OUTPUT_NAME}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
