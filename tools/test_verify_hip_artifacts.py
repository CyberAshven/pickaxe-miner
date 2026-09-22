#!/usr/bin/env python3
"""Regression tests for dependency-free PHOTON HIP artifact inspection."""

from __future__ import annotations

import copy
import io
import json
from pathlib import Path
import unittest
from contextlib import redirect_stdout

from tools import verify_hip_artifacts as verifier


ROOT = Path(__file__).resolve().parents[1]
ARTIFACTS = ROOT / "hip" / "build" / "gfx1036"
CONTRACT = json.loads((ROOT / "hip" / "kernel_contract.json").read_text(encoding="utf-8"))


class HipArtifactVerifierTests(unittest.TestCase):
    def verify_quietly(self, architecture: str, code_object: dict[str, object]) -> None:
        with redirect_stdout(io.StringIO()):
            verifier.verify_artifact(None, ARTIFACTS, architecture, code_object)

    def test_dependency_free_inspector_validates_shipped_artifacts(self) -> None:
        for code_object in CONTRACT["code_objects"]:
            with self.subTest(file=code_object["file"]):
                self.verify_quietly("gfx1036", code_object)

    def test_dependency_free_inspector_rejects_wrong_architecture(self) -> None:
        with self.assertRaisesRegex(RuntimeError, "expected AMDGPU target gfx1100"):
            self.verify_quietly("gfx1100", CONTRACT["code_objects"][0])

    def test_dependency_free_inspector_rejects_kernel_abi_drift(self) -> None:
        changed = copy.deepcopy(CONTRACT["code_objects"][0])
        changed["kernels"][0]["args"][0]["offset"] = 4
        with self.assertRaisesRegex(RuntimeError, "kernel ABI mismatch"):
            self.verify_quietly("gfx1036", changed)

    def test_messagepack_decoder_covers_amdgpu_metadata_shapes(self) -> None:
        # {"k": [1, true, -1, "v"]}
        payload = bytes((0x81, 0xA1, ord("k"), 0x94, 0x01, 0xC3, 0xFF, 0xA1, ord("v")))
        self.assertEqual(verifier.decode_msgpack(payload), {"k": [1, True, -1, "v"]})

    def test_hip_translation_units_share_the_proven_production_kernels(self) -> None:
        wrappers = {
            "stage_a_rfc6979.hip.cpp": "stage_a_rfc6979.cu",
            "photon_stage_b16.hip.cpp": "photon_stage_b16.cu",
            "photon_c1_schnorr.hip.cpp": "photon_c1_schnorr.cu",
            "stage_c_hash.hip.cpp": "stage_c_hash.cu",
        }
        for wrapper, cuda_source in wrappers.items():
            with self.subTest(wrapper=wrapper):
                lines = (ROOT / "hip" / wrapper).read_text(encoding="utf-8").splitlines()
                self.assertEqual(
                    lines,
                    [
                        "#include <hip/hip_runtime.h>",
                        f'#include "../cuda/{cuda_source}"',
                    ],
                )


if __name__ == "__main__":
    unittest.main()
