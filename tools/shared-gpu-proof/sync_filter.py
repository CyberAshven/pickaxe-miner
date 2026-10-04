"""Compile the native Rust hashing source for portable GPUs and check for drift.

#### PR #22
Generated WGSL is a build artifact, never an independently maintained engine.
CI rebuilds it with the pinned compiler and compares every byte. Use --write
after changing shared Rust source; normal Cargo builds verify its manifest.
"""
from pathlib import Path
import argparse
import hashlib
import os
import subprocess

ROOT = Path(__file__).resolve().parents[2]
OUTPUT = ROOT / "reference/shared-t2"
GENERATED = ROOT / "artifacts/shared-gpu-proof/filter"
SOURCES = (
    "rust-engine/src/sha256.rs",
    "rust-engine/src/t2_block.rs",
    "tools/shared-gpu-proof/Cargo.toml",
    "tools/shared-gpu-proof/Cargo.lock",
    "tools/shared-gpu-proof/rust-toolchain.toml",
    "tools/shared-gpu-proof/builder/Cargo.toml",
    "tools/shared-gpu-proof/builder/src/main.rs",
    "tools/shared-gpu-proof/filter/Cargo.toml",
    "tools/shared-gpu-proof/filter/src/lib.rs",
    "tools/shared-gpu-proof/sync_filter.py",
)
SHADERS = tuple(f"pickaxe_shared_t2_{i}.wgsl" for i in range(17))


def normalized(path):
    return path.read_bytes().replace(b"\r\n", b"\n")


def manifest(files):
    return "".join(f"{hashlib.sha256(data).hexdigest()}  {path}\n"
                   for path, data in sorted(files.items()))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--write", action="store_true", help="update generated shaders and provenance")
    args = parser.parse_args()
    environment = dict(os.environ, PICKAXE_BUILD_SHARED_FILTER="1")
    subprocess.run([
        "cargo", "+nightly-2026-04-11", "run", "--locked", "--release",
        "--manifest-path", "tools/shared-gpu-proof/Cargo.toml",
        "-p", "pickaxe-shared-hash-builder",
    ], cwd=ROOT, env=environment, check=True)
    files = {path: normalized(ROOT / path) for path in SOURCES}
    for name in SHADERS:
        data = normalized(GENERATED / name)
        path = OUTPUT / name
        if args.write:
            OUTPUT.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
        elif not path.is_file() or normalized(path) != data:
            raise SystemExit(f"Generated shader drift: {name}; run sync_filter.py --write")
        files[path.relative_to(ROOT).as_posix()] = data
    expected = manifest(files).encode()
    path = OUTPUT / "SHA256SUMS"
    if args.write:
        path.write_bytes(expected)
    elif not path.is_file() or normalized(path) != expected:
        raise SystemExit("Shared Rust provenance changed; run sync_filter.py --write")
    print("All 17 portable filters match the pinned shared Rust source.")


if __name__ == "__main__":
    main()
