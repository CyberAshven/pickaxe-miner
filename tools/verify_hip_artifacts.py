#!/usr/bin/env python3
"""Verify PHOTON HIP code-object architecture, symbols, and kernel ABI metadata."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import re
import shutil
import struct
import subprocess
import sys
import textwrap

ROOT = Path(__file__).resolve().parents[1]
CONTRACT_PATH = ROOT / "hip" / "kernel_contract.json"


def find_llvm_tool(name: str) -> str | None:
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
    return None


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


class MsgpackReader:
    """Minimal MessagePack decoder for AMDGPU metadata notes.

    ROCm stores ``amdhsa.*`` kernel metadata in an ELF NOTE as MessagePack.
    Keeping this small decoder here lets release artifacts be verified on hosts
    that do not have a ROCm installation or ``llvm-readelf`` available.
    """

    def __init__(self, payload: bytes):
        self.payload = payload
        self.offset = 0

    def take(self, size: int) -> bytes:
        end = self.offset + size
        if end > len(self.payload):
            raise RuntimeError("truncated MessagePack AMDGPU metadata")
        value = self.payload[self.offset : end]
        self.offset = end
        return value

    def integer(self, size: int, *, signed: bool = False) -> int:
        return int.from_bytes(self.take(size), "big", signed=signed)

    def string(self, size: int) -> str:
        try:
            return self.take(size).decode("utf-8")
        except UnicodeDecodeError as error:
            raise RuntimeError(f"invalid UTF-8 in AMDGPU metadata: {error}") from error

    def array(self, size: int) -> list[object]:
        return [self.decode() for _ in range(size)]

    def mapping(self, size: int) -> dict[object, object]:
        result: dict[object, object] = {}
        for _ in range(size):
            key = self.decode()
            try:
                hash(key)
            except TypeError as error:
                raise RuntimeError("unhashable MessagePack AMDGPU metadata key") from error
            result[key] = self.decode()
        return result

    def decode(self) -> object:
        marker = self.integer(1)
        if marker <= 0x7F:
            return marker
        if 0x80 <= marker <= 0x8F:
            return self.mapping(marker & 0x0F)
        if 0x90 <= marker <= 0x9F:
            return self.array(marker & 0x0F)
        if 0xA0 <= marker <= 0xBF:
            return self.string(marker & 0x1F)
        if marker >= 0xE0:
            return marker - 0x100
        if marker == 0xC0:
            return None
        if marker == 0xC2:
            return False
        if marker == 0xC3:
            return True
        if marker == 0xC4:
            return self.take(self.integer(1))
        if marker == 0xC5:
            return self.take(self.integer(2))
        if marker == 0xC6:
            return self.take(self.integer(4))
        if marker == 0xCA:
            return struct.unpack(">f", self.take(4))[0]
        if marker == 0xCB:
            return struct.unpack(">d", self.take(8))[0]
        if marker == 0xCC:
            return self.integer(1)
        if marker == 0xCD:
            return self.integer(2)
        if marker == 0xCE:
            return self.integer(4)
        if marker == 0xCF:
            return self.integer(8)
        if marker == 0xD0:
            return self.integer(1, signed=True)
        if marker == 0xD1:
            return self.integer(2, signed=True)
        if marker == 0xD2:
            return self.integer(4, signed=True)
        if marker == 0xD3:
            return self.integer(8, signed=True)
        if marker == 0xD9:
            return self.string(self.integer(1))
        if marker == 0xDA:
            return self.string(self.integer(2))
        if marker == 0xDB:
            return self.string(self.integer(4))
        if marker == 0xDC:
            return self.array(self.integer(2))
        if marker == 0xDD:
            return self.array(self.integer(4))
        if marker == 0xDE:
            return self.mapping(self.integer(2))
        if marker == 0xDF:
            return self.mapping(self.integer(4))
        raise RuntimeError(f"unsupported MessagePack marker 0x{marker:02x} in AMDGPU metadata")


def decode_msgpack(payload: bytes) -> object:
    reader = MsgpackReader(payload)
    value = reader.decode()
    if reader.offset != len(payload):
        raise RuntimeError(
            f"AMDGPU metadata has {len(payload) - reader.offset} trailing MessagePack bytes"
        )
    return value


def c_string(payload: bytes, offset: int) -> str:
    if offset < 0 or offset >= len(payload):
        return ""
    end = payload.find(b"\0", offset)
    if end < 0:
        end = len(payload)
    return payload[offset:end].decode("utf-8", errors="replace")


def elf_sections(payload: bytes) -> list[dict[str, object]]:
    if len(payload) < 64 or payload[:4] != b"\x7fELF":
        raise RuntimeError("not an ELF code object")
    if payload[4] != 2:
        raise RuntimeError("HIP artifact is not ELF64")
    if payload[5] != 1:
        raise RuntimeError("HIP artifact is not little-endian ELF")

    section_offset = struct.unpack_from("<Q", payload, 0x28)[0]
    section_entry_size = struct.unpack_from("<H", payload, 0x3A)[0]
    section_count = struct.unpack_from("<H", payload, 0x3C)[0]
    string_index = struct.unpack_from("<H", payload, 0x3E)[0]
    if section_entry_size < 64:
        raise RuntimeError(f"invalid ELF64 section entry size {section_entry_size}")
    if section_count == 0 or string_index >= section_count:
        raise RuntimeError("unsupported or invalid ELF section table")
    table_end = section_offset + section_entry_size * section_count
    if table_end > len(payload):
        raise RuntimeError("ELF section table exceeds artifact bounds")

    sections: list[dict[str, object]] = []
    for index in range(section_count):
        offset = section_offset + index * section_entry_size
        values = struct.unpack_from("<IIQQQQIIQQ", payload, offset)
        sections.append(
            {
                "name_offset": values[0],
                "type": values[1],
                "offset": values[4],
                "size": values[5],
                "link": values[6],
                "entry_size": values[9],
            }
        )

    string_section = sections[string_index]
    string_start = int(string_section["offset"])
    string_end = string_start + int(string_section["size"])
    if string_end > len(payload):
        raise RuntimeError("ELF section-name table exceeds artifact bounds")
    string_table = payload[string_start:string_end]
    for section in sections:
        section["name"] = c_string(string_table, int(section["name_offset"]))
        start = int(section["offset"])
        end = start + int(section["size"])
        if section["type"] != 8 and end > len(payload):  # SHT_NOBITS has no file payload.
            raise RuntimeError(f"ELF section {section['name']!r} exceeds artifact bounds")
    return sections


def metadata_from_msgpack(value: object) -> tuple[str, dict[str, dict[str, object]]]:
    if not isinstance(value, dict):
        raise RuntimeError("AMDGPU metadata note is not a MessagePack map")
    target = value.get("amdhsa.target")
    kernel_values = value.get("amdhsa.kernels")
    if not isinstance(target, str) or not target:
        raise RuntimeError("AMDGPU metadata is missing amdhsa.target")
    if not isinstance(kernel_values, list):
        raise RuntimeError("AMDGPU metadata is missing amdhsa.kernels")

    parsed: dict[str, dict[str, object]] = {}
    for kernel in kernel_values:
        if not isinstance(kernel, dict):
            raise RuntimeError("AMDGPU kernel metadata entry is not a map")
        name = kernel.get(".name")
        if not isinstance(name, str) or not name:
            raise RuntimeError("AMDGPU kernel metadata entry is missing .name")
        raw_args = kernel.get(".args", [])
        if not isinstance(raw_args, list):
            raise RuntimeError(f"AMDGPU kernel {name} has invalid .args metadata")
        args: list[dict[str, object]] = []
        for raw_arg in raw_args:
            if not isinstance(raw_arg, dict):
                raise RuntimeError(f"AMDGPU kernel {name} has a non-map argument entry")
            arg: dict[str, object] = {}
            for source, destination in (
                (".offset", "offset"),
                (".size", "size"),
                (".value_kind", "value_kind"),
            ):
                if source in raw_arg:
                    arg[destination] = raw_arg[source]
            args.append(arg)
        parsed[name] = {
            "kernarg_segment_size": kernel.get(".kernarg_segment_size"),
            "args": args,
        }
    return target, parsed


def inspect_elf_metadata(path: Path) -> tuple[str, dict[str, dict[str, object]], set[str]]:
    payload = path.read_bytes()
    sections = elf_sections(payload)
    metadata: tuple[str, dict[str, dict[str, object]]] | None = None

    for section in sections:
        if section["type"] != 7:  # SHT_NOTE
            continue
        start = int(section["offset"])
        end = start + int(section["size"])
        cursor = start
        while cursor + 12 <= end:
            name_size, desc_size, _note_type = struct.unpack_from("<III", payload, cursor)
            cursor += 12
            name_end = cursor + name_size
            if name_end > end:
                raise RuntimeError(f"{path}: malformed ELF NOTE owner")
            owner = payload[cursor:name_end].rstrip(b"\0")
            cursor += (name_size + 3) & ~3
            desc_end = cursor + desc_size
            if desc_end > end:
                raise RuntimeError(f"{path}: malformed ELF NOTE descriptor")
            descriptor = payload[cursor:desc_end]
            cursor += (desc_size + 3) & ~3
            if owner != b"AMDGPU" or b"amdhsa.kernels" not in descriptor:
                continue
            decoded = decode_msgpack(descriptor)
            metadata = metadata_from_msgpack(decoded)
            break
        if metadata is not None:
            break
    if metadata is None:
        raise RuntimeError(f"{path}: no AMDGPU MessagePack kernel metadata note found")

    symbols: set[str] = set()
    for section in sections:
        if section["type"] not in {2, 11}:  # SHT_SYMTAB / SHT_DYNSYM
            continue
        link = int(section["link"])
        if link >= len(sections):
            raise RuntimeError(f"{path}: ELF symbol table has invalid string-table link")
        string_section = sections[link]
        strings_start = int(string_section["offset"])
        strings_end = strings_start + int(string_section["size"])
        strings = payload[strings_start:strings_end]
        entry_size = int(section["entry_size"]) or 24
        if entry_size < 24:
            raise RuntimeError(f"{path}: invalid ELF64 symbol entry size {entry_size}")
        start = int(section["offset"])
        end = start + int(section["size"])
        for cursor in range(start, end, entry_size):
            if cursor + 24 > end:
                break
            name_offset = struct.unpack_from("<I", payload, cursor)[0]
            name = c_string(strings, name_offset)
            if name:
                symbols.add(name)

    target, kernels = metadata
    return target, kernels, symbols


def verify_artifact(
    readelf: str | None,
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

    if readelf is None:
        target, kernels, symbols = inspect_elf_metadata(path)
        inspector = "python-elf"
    else:
        target, kernels = parse_metadata(metadata_yaml(readelf, path))
        symbol_output = run_text([readelf, "--symbols", str(path)])
        symbols = {
            token
            for line in symbol_output.splitlines()
            for token in line.split()
            if token and not token.isdigit()
        }
        inspector = "llvm-readelf"
    target_match = re.fullmatch(
        rf"amdgcn-amd-amdhsa--{re.escape(architecture)}(?::[A-Za-z0-9_+:-]+)?",
        target,
    )
    if target_match is None:
        raise RuntimeError(
            f"{path}: expected AMDGPU target {architecture}, metadata reports {target!r}"
        )

    for expected_kernel in code_object["kernels"]:
        symbol = str(expected_kernel["symbol"])
        actual = kernels.get(symbol)
        if actual is None:
            raise RuntimeError(f"{path}: missing production kernel metadata for {symbol}")
        if symbol not in symbols and f"{symbol}.kd" not in symbols:
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

    print(
        f"verified {filename}: target={architecture}, production kernels/ABI=ok, "
        f"inspector={inspector}"
    )


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
        if readelf is None:
            print(
                "llvm-readelf unavailable; using dependency-free ELF/AMDGPU metadata inspector"
            )
        for code_object in contract["code_objects"]:
            verify_artifact(readelf, directory, args.arch, code_object)
    except (OSError, RuntimeError, subprocess.CalledProcessError, ValueError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1

    print(f"verified complete PHOTON HIP artifact set in {directory}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
