"""Compile the native Rust GPU source for portable GPUs and check for drift.

#### PR #22
Generated WGSL is a build artifact, never an independently maintained engine.
CI rebuilds it with the pinned compiler and compares every byte. Use --write
after changing shared Rust source; normal Cargo builds verify its manifests.
Outputs: the 17 T2 filters (reference/shared-t2) and the signing stages
(reference/shared-signer).
"""
from pathlib import Path
import argparse
import hashlib
import os
import subprocess

ROOT = Path(__file__).resolve().parents[2]
COMMON = (
    "tools/shared-gpu-proof/Cargo.toml",
    "tools/shared-gpu-proof/Cargo.lock",
    "tools/shared-gpu-proof/rust-toolchain.toml",
    "tools/shared-gpu-proof/builder/Cargo.toml",
    "tools/shared-gpu-proof/builder/src/main.rs",
    "tools/shared-gpu-proof/sync_filter.py",
)
TARGETS = (
    {
        "name": "filter",
        "environment": "PICKAXE_BUILD_SHARED_FILTER",
        "output": "reference/shared-t2",
        "sources": (
            "rust-engine/src/sha256.rs",
            "rust-engine/src/t2_block.rs",
            "tools/shared-gpu-proof/filter/Cargo.toml",
            "tools/shared-gpu-proof/filter/src/lib.rs",
        ),
        "shaders": tuple(f"pickaxe_shared_t2_{i}.wgsl" for i in range(17)),
    },
    {
        "name": "signer",
        "environment": "PICKAXE_BUILD_SHARED_SIGNER",
        "output": "reference/shared-signer",
        "sources": (
            "rust-engine/src/field.rs",
            "rust-engine/src/point.rs",
            "rust-engine/src/scalar.rs",
            "rust-engine/src/sha256.rs",
            "rust-engine/src/sign.rs",
            "rust-engine/src/wide.rs",
            "tools/shared-gpu-proof/signer/Cargo.toml",
            "tools/shared-gpu-proof/signer/src/lib.rs",
        ),
        "shaders": ("pickaxe_shared_signer.wgsl",),
    },
)


def normalized(path):
    return path.read_bytes().replace(b"\r\n", b"\n")


def manifest(files):
    return "".join(f"{hashlib.sha256(data).hexdigest()}  {path}\n"
                   for path, data in sorted(files.items()))


def sync(target, write):
    # The fully inlined signing stages exceed rustc's default 8 MiB stack.
    environment = dict(os.environ, RUST_MIN_STACK="536870912", **{target["environment"]: "1"})
    subprocess.run([
        "cargo", "+nightly-2026-04-11", "run", "--locked", "--release",
        "--manifest-path", "tools/shared-gpu-proof/Cargo.toml",
        "-p", "pickaxe-shared-hash-builder",
    ], cwd=ROOT, env=environment, check=True)
    generated = ROOT / "artifacts/shared-gpu-proof" / target["name"]
    output = ROOT / target["output"]
    files = {path: normalized(ROOT / path) for path in target["sources"] + COMMON}
    for name in target["shaders"]:
        data = normalized(generated / name)
        path = output / name
        if write:
            output.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
        elif not path.is_file() or normalized(path) != data:
            raise SystemExit(f"Generated shader drift: {name}; run sync_filter.py --write")
        files[path.relative_to(ROOT).as_posix()] = data
    expected = manifest(files).encode()
    path = output / "SHA256SUMS"
    if write:
        path.write_bytes(expected)
    elif not path.is_file() or normalized(path) != expected:
        raise SystemExit(f"Shared Rust provenance changed ({target['name']}); run sync_filter.py --write")
    print(f"All {len(target['shaders'])} portable {target['name']} shaders match the pinned shared Rust source.")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--write", action="store_true", help="update generated shaders and provenance")
    args = parser.parse_args()
    for target in TARGETS:
        sync(target, args.write)


if __name__ == "__main__":
    main()
