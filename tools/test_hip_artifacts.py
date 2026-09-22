#!/usr/bin/env python3
"""Offline regression tests for HIP artifact validation; no GPU is required."""

from __future__ import annotations

import contextlib
import copy
import io
import json
from pathlib import Path
import struct
import tempfile
import unittest
from unittest.mock import patch

import build_hip
import verify_hip_artifacts as verifier

CONTRACT = json.loads(verifier.CONTRACT_PATH.read_text(encoding="utf-8"))
ARCH = CONTRACT["architecture"]
TARGET = f"hipv4-amdgcn-amd-amdhsa--{ARCH}"


def elf_fixture(**overrides: int) -> bytes:
    # Synthetic header/tables only. These are parser fixtures, NOT compiled HSACO.
    values = dict(elf_class=2, endian=1, ident_version=1, osabi=64, abi=3,
                  file_type=3, machine=224, version=1, flags=0x45,
                  ehsize=64, phoff=64, phentsize=56, phnum=1,
                  shoff=120, shentsize=64, shnum=1)
    values.update(overrides)
    ident = b"\x7fELF" + bytes(values[key] for key in (
        "elf_class", "endian", "ident_version", "osabi", "abi")) + bytes(7)
    return struct.pack(
        "<16sHHIQQQIHHHHHH", ident, values["file_type"], values["machine"],
        values["version"], 0, values["phoff"], values["shoff"], values["flags"],
        values["ehsize"], values["phentsize"], values["phnum"],
        values["shentsize"], values["shnum"], 0,
    ) + bytes(120)


def bundle_fixture(entries: list[tuple[str, bytes]]) -> bytes:
    header = build_hip.CLANG_OFFLOAD_BUNDLE_MAGIC + struct.pack("<Q", len(entries))
    offset = len(header) + sum(24 + len(target.encode()) for target, _ in entries)
    body = b""
    for target, payload in entries:
        encoded = target.encode()
        header += struct.pack("<QQQ", offset, len(payload), len(encoded)) + encoded
        offset += len(payload)
        body += payload
    return header + body


def metadata_fixture(code_object: dict, target: str | None = None) -> str:
    lines = ["amdhsa.kernels:"]
    for kernel in code_object["kernels"]:
        lines.append("  - .args:")
        for arg in kernel["args"]:
            kind = "global_buffer" if arg["kind"] == "ptr" else "by_value"
            lines.extend((f"      - .offset: {arg['offset']}",
                          f"        .size: {arg['size']}",
                          f"        .value_kind: {kind}"))
        end = max(arg["offset"] + arg["size"] for arg in kernel["args"])
        lines.extend((f"    .kernarg_segment_size: {end}",
                      f"    .name: {kernel['symbol']}"))
    lines.extend((f"amdhsa.target: '{target or f'amdgcn-amd-amdhsa--{ARCH}'}'",
                  "amdhsa.version:", "  - 1", "  - 2"))
    return "\n".join(lines) + "\n"


def symbols_fixture(code_object: dict) -> str:
    lines = []
    for i, kernel in enumerate(code_object["kernels"]):
        symbol = kernel["symbol"]
        lines.append(f" {2*i+1}: 0000000000001800 256 FUNC GLOBAL PROTECTED 6 {symbol}")
        lines.append(f" {2*i+2}: 0000000000001000 64 OBJECT GLOBAL DEFAULT 5 {symbol}.kd")
    return "\n".join(lines) + "\n"


class BundleTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.path = Path(self.tmp.name) / "kernel.hsaco"

    def normalize(self, data: bytes) -> bytes:
        self.path.write_bytes(data)
        with contextlib.redirect_stdout(io.StringIO()):
            build_hip.normalize_code_object(self.path, ARCH)
        return self.path.read_bytes()

    def rejected_without_replacement(self, data: bytes) -> None:
        self.path.write_bytes(data)
        with self.assertRaises(RuntimeError):
            build_hip.normalize_code_object(self.path, ARCH)
        self.assertEqual(self.path.read_bytes(), data)
        self.assertFalse(self.path.with_suffix(".hsaco.tmp").exists())

    def test_raw_elf_is_preserved(self) -> None:
        self.assertEqual(self.normalize(elf_fixture()), elf_fixture())

    def test_extracts_exact_device_and_empty_host(self) -> None:
        data = bundle_fixture([("host-x86_64-unknown-linux", b""), (TARGET, elf_fixture())])
        self.assertEqual(self.normalize(data), elf_fixture())

    def test_legacy_hip_offload_kind(self) -> None:
        data = bundle_fixture([(TARGET.replace("hipv4-", "hip-"), elf_fixture())])
        self.assertEqual(self.normalize(data), elf_fixture())

    def test_valid_feature_suffix(self) -> None:
        self.assertEqual(self.normalize(bundle_fixture([(TARGET + ":xnack-", elf_fixture())])), elf_fixture())

    def test_gfx_prefix_collision_rejected(self) -> None:
        self.rejected_without_replacement(bundle_fixture([(TARGET + "0", elf_fixture())]))

    def test_non_hip_offload_kind_rejected(self) -> None:
        self.rejected_without_replacement(bundle_fixture([(TARGET.replace("hipv4-", "openmp-"), elf_fixture())]))

    def test_malformed_feature_suffix_rejected(self) -> None:
        for suffix in (":", ":xnack", ":xnack--", "/suffix"):
            with self.subTest(suffix=suffix):
                self.rejected_without_replacement(bundle_fixture([(TARGET + suffix, elf_fixture())]))

    def test_ambiguous_matching_devices_rejected(self) -> None:
        self.rejected_without_replacement(bundle_fixture([(TARGET, elf_fixture()), (TARGET + ":xnack-", elf_fixture())]))

    def test_wrong_architecture_rejected(self) -> None:
        self.rejected_without_replacement(bundle_fixture([(TARGET.replace(ARCH, "gfx1030"), elf_fixture())]))

    def test_bundle_truncations_rejected(self) -> None:
        original = bundle_fixture([(TARGET, elf_fixture())])
        magic_size = len(build_hip.CLANG_OFFLOAD_BUNDLE_MAGIC)
        for end in (0, 4, magic_size, magic_size + 7, magic_size + 15,
                    magic_size + 32, len(original) - 1):
            with self.subTest(end=end):
                self.rejected_without_replacement(original[:end])

    def test_empty_bundle_rejected(self) -> None:
        self.rejected_without_replacement(bundle_fixture([]))

    def test_non_elf_device_rejected(self) -> None:
        self.rejected_without_replacement(bundle_fixture([(TARGET, b"not ELF")]))

    def test_truncated_raw_elf_rejected(self) -> None:
        self.rejected_without_replacement(b"\x7fELF")

    def test_non_hsa_raw_elf_rejected(self) -> None:
        self.rejected_without_replacement(elf_fixture(osabi=0))

    def test_overlapping_bundle_payloads_rejected(self) -> None:
        host = "host-x86_64-unknown-linux"
        payload = bytearray(bundle_fixture([(host, elf_fixture()), (TARGET, elf_fixture())]))
        first_descriptor = len(build_hip.CLANG_OFFLOAD_BUNDLE_MAGIC) + 8
        second_descriptor = first_descriptor + 24 + len(host)
        first_offset = struct.unpack_from("<Q", payload, first_descriptor)[0]
        struct.pack_into("<Q", payload, second_descriptor, first_offset)
        self.rejected_without_replacement(bytes(payload))

    def test_invalid_target_utf8_rejected_cleanly(self) -> None:
        payload = bytearray(bundle_fixture([(TARGET, elf_fixture())]))
        payload[len(build_hip.CLANG_OFFLOAD_BUNDLE_MAGIC) + 8 + 24] = 0xff
        self.rejected_without_replacement(bytes(payload))


class ArtifactTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.directory = Path(self.tmp.name)
        self.code = copy.deepcopy(CONTRACT["code_objects"][0])
        self.path = self.directory / self.code["file"]
        self.path.write_bytes(elf_fixture())

    def verify(self, *, payload: bytes | None = None, metadata: str | None = None,
               symbols: str | None = None, code: dict | None = None) -> None:
        if payload is not None:
            self.path.write_bytes(payload)
        with patch.object(verifier, "metadata_yaml", return_value=metadata if metadata is not None else metadata_fixture(self.code)), \
             patch.object(verifier, "run_text", return_value=symbols if symbols is not None else symbols_fixture(self.code)), \
             contextlib.redirect_stdout(io.StringIO()):
            verifier.verify_artifact("llvm-readelf", self.directory, ARCH, code or self.code)

    def test_valid_header_symbols_and_abi(self) -> None:
        self.verify()

    def test_rejects_invalid_header_fields(self) -> None:
        cases = ({"elf_class": 1}, {"endian": 2}, {"ident_version": 0},
                 {"osabi": 0}, {"abi": 0}, {"abi": 255},
                 {"file_type": 1}, {"file_type": 2}, {"machine": 62},
                 {"version": 0}, {"ehsize": 0})
        for case in cases:
            with self.subTest(case=case), self.assertRaises(RuntimeError):
                self.verify(payload=elf_fixture(**case))

    def test_rejects_truncated_or_missing_tables(self) -> None:
        for case in ({"phoff": 9999}, {"shoff": 9999}, {"phnum": 0},
                     {"shnum": 0}, {"phentsize": 1}, {"shentsize": 1}):
            with self.subTest(case=case), self.assertRaises(RuntimeError):
                self.verify(payload=elf_fixture(**case))

    def test_header_processor_must_match_metadata(self) -> None:
        with self.assertRaises(RuntimeError):
            self.verify(payload=elf_fixture(flags=0x36))

    def test_wrong_metadata_target_or_prefix_rejected(self) -> None:
        for target in ("amdgcn-amd-amdhsa--gfx1030", "amdgcn-amd-amdhsa--gfx10360"):
            with self.subTest(target=target), self.assertRaises(RuntimeError):
                self.verify(metadata=metadata_fixture(self.code, target))

    def test_no_metadata_rejected(self) -> None:
        with self.assertRaises(RuntimeError):
            self.verify(metadata="")

    def test_duplicate_target_rejected(self) -> None:
        duplicate = metadata_fixture(self.code) + "amdhsa.target: 'amdgcn-amd-amdhsa--gfx1030'\n"
        with self.assertRaises(RuntimeError):
            self.verify(metadata=duplicate)

    def test_duplicate_kernel_rejected(self) -> None:
        duplicate = copy.deepcopy(self.code)
        duplicate["kernels"] *= 2
        with self.assertRaises(RuntimeError):
            self.verify(metadata=metadata_fixture(duplicate))

    def test_undefined_kernel_rejected(self) -> None:
        with self.assertRaises(RuntimeError):
            self.verify(symbols=symbols_fixture(self.code).replace("PROTECTED 6", "PROTECTED UND"))

    def test_missing_descriptor_rejected(self) -> None:
        with self.assertRaises(RuntimeError):
            self.verify(symbols=symbols_fixture(self.code).splitlines()[0])

    def test_descriptor_without_entry_rejected(self) -> None:
        with self.assertRaises(RuntimeError):
            self.verify(symbols=symbols_fixture(self.code).splitlines()[1])

    def test_zero_length_kernel_symbol_rejected(self) -> None:
        with self.assertRaises(RuntimeError):
            self.verify(symbols=symbols_fixture(self.code).replace(" 256 FUNC ", " 0 FUNC "))

    def test_abs_symbol_is_not_a_code_section(self) -> None:
        with self.assertRaises(RuntimeError):
            self.verify(symbols=symbols_fixture(self.code).replace("PROTECTED 6", "PROTECTED ABS"))

    def test_missing_kernel_metadata_rejected(self) -> None:
        text = metadata_fixture(self.code).replace(self.code["kernels"][0]["symbol"], "other_kernel")
        with self.assertRaises(RuntimeError):
            self.verify(metadata=text)

    def test_offset_size_and_kind_mismatch_rejected(self) -> None:
        text = metadata_fixture(self.code)
        for changed in (text.replace(".offset: 8", ".offset: 9", 1),
                        text.replace(".size: 8", ".size: 4", 1),
                        text.replace(".value_kind: by_value", ".value_kind: global_buffer", 1)):
            with self.subTest(changed=changed), self.assertRaises(RuntimeError):
                self.verify(metadata=changed)

    def test_missing_argument_offset_rejected(self) -> None:
        text = metadata_fixture(self.code).replace(".offset: 8", ".not_offset: 8", 1)
        with self.assertRaises(RuntimeError):
            self.verify(metadata=text)

    def test_short_kernarg_segment_rejected(self) -> None:
        text = metadata_fixture(self.code).replace(".kernarg_segment_size: 44", ".kernarg_segment_size: 8")
        with self.assertRaises(RuntimeError):
            self.verify(metadata=text)

    def test_complete_four_file_set_and_all_seven_kernels(self) -> None:
        for code in CONTRACT["code_objects"]:
            with self.subTest(file=code["file"]):
                self.code = copy.deepcopy(code)
                self.path = self.directory / self.code["file"]
                self.path.write_bytes(elf_fixture())
                self.verify()
        self.assertEqual(len(CONTRACT["code_objects"]), 4)
        self.assertEqual(sum(len(obj["kernels"]) for obj in CONTRACT["code_objects"]), 7)

    def test_missing_any_required_file_rejected(self) -> None:
        for absent in CONTRACT["code_objects"]:
            with self.subTest(absent=absent["file"]):
                absent_path = self.directory / absent["file"]
                if absent_path.exists():
                    absent_path.unlink()
                with self.assertRaisesRegex(RuntimeError, "missing HIP code object"):
                    self.verify(code=absent)

    def test_metadata_yaml_missing_or_unterminated_rejected(self) -> None:
        for text in ("no notes", "---\namdhsa.target: foo"):
            with self.subTest(text=text), patch.object(verifier, "run_text", return_value=text), \
                 self.assertRaises(RuntimeError):
                verifier.metadata_yaml("llvm-readelf", self.path)


class BuildCliTests(unittest.TestCase):
    def test_invalid_architecture_is_rejected_before_compiler(self) -> None:
        for arch in ("gfx1036/../../elsewhere", "gfx1036\\..\\x", "gfx1036:bad", "gfx"):
            with self.subTest(arch=arch), patch("sys.argv", ["build_hip.py", "--arch", arch]), \
                 patch.object(build_hip, "find_compiler") as find, \
                 contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as error:
                build_hip.main()
            self.assertEqual(error.exception.code, 2)
            find.assert_not_called()


if __name__ == "__main__":
    unittest.main()
