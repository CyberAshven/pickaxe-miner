#!/usr/bin/env python3
"""Verify PHOTON HIP code-object architecture, symbols, and kernel ABI metadata."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import textwrap

ROOT = Path(__file__).resolve().parents[1]
CONTRACT_PATH = ROOT / "hip" / "kernel_contract.json"


def find_llvm_tool(name: str) -> str:
    explicit = os.environ.get(name.upper().replace("-", "_"))
    if explicit:
        return explicit

    found = shutil.which(name)
    if found:
        return found

    candidates: list[Path] = []
    hip_path = os.environ.get("HIP_PATH")
    if hip_path:
        root = Path(hip_path)
        candidates.extend(
            (
                root / "lib" / "llvm" / "bin" / name,
                root / "llvm" / "bin" / name,
                root / "bin" / name,
            )
        )
    candidates.extend(
        (
            Path("/opt/rocm/lib/llvm/bin") / name,
            Path("/opt/rocm/llvm/bin") / name,
            Path("/opt/rocm/bin") / name,
        )
    )
    if os.name == "nt":
        candidates = [path.with_suffix(".exe") for path in candidates]

    for candidate in candidates:
        if candidate.is_file():
            return str(candidate)
    raise RuntimeError(f"unable to find {name}; set HIP_PATH or add ROCm LLVM tools to PATH")


def run_text(command: list[str]) -> str:
    result = subprocess.run(
        command,
        check=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        encoding="utf-8",
        errors="replace",
    )
    return result.stdout


def metadata_yaml(readelf: str, path: Path) -> str:
    output = run_text([readelf, "--notes", str(path)])
    lines = output.splitlines()
    start = next((index for index, line in enumerate(lines) if line.strip() == "---"), None)
    if start is None:
        raise RuntimeError(f"{path}: llvm-readelf did not expose AMDGPU metadata YAML")
    end = next(
        (index for index in range(start + 1, len(lines)) if lines[index].strip() == "..."),
        None,
    )
    if end is None:
        raise RuntimeError(f"{path}: unterminated AMDGPU metadata YAML")
    return textwrap.dedent("\n".join(lines[start + 1 : end]))


def scalar(line: str) -> str:
    return line.split(":", 1)[1].strip().strip("'\"")


def parse_metadata(yaml_text: str) -> tuple[str, dict[str, dict[str, object]]]:
    lines = yaml_text.splitlines()
    target = ""
    for line in lines:
        if re.match(r"^amdhsa\.target\s*:", line):
            target = scalar(line)
            break
    if not target:
        raise RuntimeError("AMDGPU metadata is missing amdhsa.target")

    kernels_start = next(
        (index for index, line in enumerate(lines) if line.strip() == "amdhsa.kernels:"),
        None,
    )
    if kernels_start is None:
        raise RuntimeError("AMDGPU metadata is missing amdhsa.kernels")

    kernel_lines: list[list[str]] = []
    current: list[str] = []
    for line in lines[kernels_start + 1 :]:
        if re.match(r"^[^ ]", line):
            break
        if re.match(r"^  - ", line):
            if current:
                kernel_lines.append(current)
            current = [line]
        elif current:
            current.append(line)
    if current:
        kernel_lines.append(current)

    parsed: dict[str, dict[str, object]] = {}
    for block in kernel_lines:
        name = ""
        kernarg_segment_size: int | None = None
        args: list[dict[str, object]] = []
        in_args = False
        current_arg: dict[str, object] | None = None
        for line in block:
            if re.match(r"^  - \.args\s*:", line) or re.match(
                r"^    \.args\s*:", line
            ):
                in_args = True
                continue
            if in_args and re.match(r"^    \.[A-Za-z_]", line):
                if current_arg is not None:
                    args.append(current_arg)
                    current_arg = None
                in_args = False
            if in_args:
                if re.match(r"^      - ", line):
                    if current_arg is not None:
                        args.append(current_arg)
                    current_arg = {}
                    property_text = line[8:]
                    if property_text.startswith(".offset:"):
                        current_arg["offset"] = int(scalar(property_text))
                    elif property_text.startswith(".size:"):
                        current_arg["size"] = int(scalar(property_text))
                    elif property_text.startswith(".value_kind:"):
                        current_arg["value_kind"] = scalar(property_text)
                    continue
                if current_arg is not None:
                    stripped = line.strip()
                    if stripped.startswith(".offset:"):
                        current_arg["offset"] = int(scalar(stripped))
                    elif stripped.startswith(".size:"):
                        current_arg["size"] = int(scalar(stripped))
                    elif stripped.startswith(".value_kind:"):
                        current_arg["value_kind"] = scalar(stripped)
                continue

            if re.match(r"^    \.name\s*:", line):
                name = scalar(line)
            elif re.match(r"^    \.kernarg_segment_size\s*:", line):
                kernarg_segment_size = int(scalar(line))

        if current_arg is not None:
            args.append(current_arg)
        if name:
            parsed[name] = {
                "kernarg_segment_size": kernarg_segment_size,
                "args": args,
            }
    return target, parsed


def verify_artifact(
    readelf: str,
    directory: Path,
    architecture: str,
    code_object: dict[str, object],
) -> None:
    filename = str(code_object["file"])
    path = directory / filename
    if not path.is_file():
        raise RuntimeError(f"missing HIP code object: {path}")
    payload = path.read_bytes()
    if len(payload) < 4 or payload[:4] != b"\x7fELF":
        raise RuntimeError(f"{path}: not an ELF HIP code object")

    target, kernels = parse_metadata(metadata_yaml(readelf, path))
    target_match = re.fullmatch(
        rf"amdgcn-amd-amdhsa--{re.escape(architecture)}(?::[A-Za-z0-9_+:-]+)?",
        target,
    )
    if target_match is None:
        raise RuntimeError(
            f"{path}: expected AMDGPU target {architecture}, metadata reports {target!r}"
        )

    symbol_output = run_text([readelf, "--symbols", str(path)])
    for expected_kernel in code_object["kernels"]:
        symbol = str(expected_kernel["symbol"])
        actual = kernels.get(symbol)
        if actual is None:
            raise RuntimeError(f"{path}: missing production kernel metadata for {symbol}")
        if not re.search(
            rf"(?m)(?:^|\s){re.escape(symbol)}(?:\.kd)?(?:\s|$)", symbol_output
        ):
            raise RuntimeError(f"{path}: missing production ELF symbol {symbol}")

        expected_args = [
            {
                "offset": int(arg["offset"]),
                "size": int(arg["size"]),
                "kind": str(arg["kind"]),
            }
            for arg in expected_kernel["args"]
        ]
        actual_args = []
        for arg in actual["args"]:
            value_kind = str(arg.get("value_kind", ""))
            if value_kind.startswith("hidden_"):
                continue
            if "offset" not in arg or "size" not in arg:
                raise RuntimeError(f"{path}:{symbol}: explicit kernel arg lacks offset/size")
            if value_kind == "by_value" and int(arg["size"]) == 4:
                kind = "u32"
            elif value_kind in {"global_buffer", "dynamic_shared_pointer"} and int(arg["size"]) == 8:
                kind = "ptr"
            else:
                raise RuntimeError(
                    f"{path}:{symbol}: unsupported explicit HIP arg kind {value_kind!r} "
                    f"with size {arg['size']}"
                )
            actual_args.append(
                {"offset": int(arg["offset"]), "size": int(arg["size"]), "kind": kind}
            )
        if actual_args != expected_args:
            raise RuntimeError(
                f"{path}:{symbol}: kernel ABI mismatch: actual {actual_args}, "
                f"expected {expected_args}"
            )

        explicit_end = max((arg["offset"] + arg["size"] for arg in actual_args), default=0)
        segment_size = actual["kernarg_segment_size"]
        if not isinstance(segment_size, int) or segment_size < explicit_end:
            raise RuntimeError(
                f"{path}:{symbol}: kernarg segment {segment_size!r} does not cover "
                f"explicit ABI ending at byte {explicit_end}"
            )

    print(f"verified {filename}: target={architecture}, production kernels/ABI=ok")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--arch", required=True, help="AMD GPU target, e.g. gfx1036")
    parser.add_argument(
        "--directory",
        type=Path,
        help="directory containing HSACO files; defaults to hip/build/<arch>",
    )
    args = parser.parse_args()

    contract = json.loads(CONTRACT_PATH.read_text(encoding="utf-8"))
    if args.arch != contract["architecture"]:
        print(
            f"error: contract is for {contract['architecture']}, requested {args.arch}",
            file=sys.stderr,
        )
        return 2
    directory = args.directory or (ROOT / "hip" / "build" / args.arch)
    try:
        readelf = find_llvm_tool("llvm-readelf")
        for code_object in contract["code_objects"]:
            verify_artifact(readelf, directory, args.arch, code_object)
    except (OSError, RuntimeError, subprocess.CalledProcessError, ValueError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1

    print(f"verified complete PHOTON HIP artifact set in {directory}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
