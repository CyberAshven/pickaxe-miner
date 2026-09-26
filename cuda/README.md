# CUDA kernels (Pickaxe)

## Diagnostic Stage A — HASH256
- Source: `stage_a_hash256.cu`
- PTX: `build/stage_a_hash256.ptx` (`sm_120`)
- Launcher: `src/cuda_stage_a.rs`
- Status: GPU == host verified on RTX 5070 Ti
- Scope: reference/diagnostic coverage only; production mining uses the persistent PHOTON pipeline below.

## Stage B — k*G (secp256k1)
- Source: `stage_b_kg.cu` (Jacobian double-and-add, 8×u32 limbs, MSVC-safe)
- PTX: `build/stage_b_kg.ptx` (`sm_120`, `-maxrregcount=128`, 1-thread blocks for register budget)
- Launcher: `src/cuda_stage_b.rs`
- Status: GPU == host (`secp256k1`) verified on RTX 5070 Ti
- Scope: reference/diagnostic coverage only; production fixed-base multiplication uses `photon_stage_b16.cu`.

## Stage C — completed PHOTON transaction HASH256 + target filter
- Source: `stage_c_hash.cu`
- PTX: `build/stage_c_hash.ptx` (`sm_120`)
- Launcher: `src/cuda_stage_c.rs`
- Semantics: exact 615-byte PHOTON transaction, nonce at bytes 390..393, signature at 426..489, strict little-endian `HASH256 < target` (a subset of the covenant rule; HIP only)
- Host boundary: bounded winner count/nonces/HASH256 only; no full candidate digest readback
- Status: exact filter reused by the persistent A→B→C engine; runtime production search remains gated on live job-generation/stale-winner integration

## Persistent PHOTON A→B→C engine
- Launcher: `src/cuda_photon.rs`
- Stage A: existing exact `stage_a_rfc6979.ptx`; message SHA-256 and BCH RFC6979 stay on-device
- Stage B: `photon_stage_b16.cu` / `build/photon_stage_b16.ptx`; M45-style 4+4+4+4 fixed-base windows
- Fixed-base table: exact M67.38 M29 16-bit format, 67,108,864 bytes, generated locally and accepted only when SHA-256 is `f6238556c4cf380be0479c14511c97cf996290eed88ce220c9dd28301c87b0e1`
- C1: `photon_c1_schnorr.cu` / `build/photon_c1_schnorr.ptx`; Jacobian normalization, challenge, fixed-private-key M39 `e*d`, and R||s all run on-device. CUDA uses `pickaxe_photon_c1_schnorr_dual`, which writes `s` for both nonce signs (`k + e*d` and `(n - k) + e*d`) and skips the per-candidate quadratic-residue test; HIP still uses `pickaxe_photon_c1_schnorr`, which applies the residue rule in C1
- C2/C3: CUDA uses `photon_c3_dual.cu` / `build/photon_c3_dual.ptx`, one entry point per baton-age layout: `pickaxe_stage_c_dual_filter` (age 0..=16, 615 bytes) and `_shift1`/`_shift2`/`_shift3` (616/617/618 bytes; everything after the age push moves by the shift). The target test is the covenant's `ABS(BIN2NUM(HASH256)) < target`, which ignores digest bit 255. It resumes SHA-256 from a per-job midstate of transaction bytes 0..383, hashes both signature variants, and runs the BCH residue test (on `Y*Z`, no inversion) only for a candidate whose variant meets the target, emitting it only if that variant is the real signature. HIP still uses the single-variant `stage_c_hash.cu` filter
- Field arithmetic (`stage_b_kg.cu`): on CUDA, multiply and square are a 32-bit column product using PTX `mad.lo.cc`/`madc.hi.cc` carry chains and a carry-chain `2^256 = 2^32 + 977` fold; HIP and host builds use the portable column-sum C++ path with the same fold. Inversion and the residue test use the libsecp256k1 addition chains
- Host boundary: only winner count and a configured bounded winner nonce/HASH256 array; no per-candidate message, RFC6979 scalar, point, signature, or digest readback
- Resources: one CUDA context/stream plus persistent table and candidate buffers per engine; host table bytes are dropped after the one-time upload
- Production integration: connected to immutable live job generations, CPU reconstruction/verification of returned winners, authoritative freshness recheck, durable submission journaling, and self-funded settlement. Real-mainnet natural-winner evidence remains an external validation item.

## Build (Windows)

The opt-in CUDA incremental search candidate is built with
`tools\build-incremental-k.cmd` and Cargo `--features incremental-k`.
Place its six PTX files beside the executable under `cuda\build`.
It walks public signing scalars using only the worker's unfunded search key,
rotates that key after each 2^32 sweep, and retains RFC6979 for reward signing.
CUDA C1 uses Montgomery multiplication with the existing fixed-key table;
HIP retains its table algorithm. The build helper also emits the test-only
`scalar-check.ptx` for independent scalar arithmetic verification.
The default build retains the pipeline above. Measurements, live validation
and serial reproduction steps are in [the experiment report](../docs/incremental-k.md).

```bat
call "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat"
nvcc -ptx -O3 -arch=sm_120 -o cuda\build\stage_a_rfc6979.ptx cuda\stage_a_rfc6979.cu
nvcc -ptx -O3 -arch=sm_120 -maxrregcount=128 -o cuda\build\stage_b_kg.ptx cuda\stage_b_kg.cu
nvcc -ptx -O3 -arch=sm_120 -maxrregcount=128 -o cuda\build\photon_stage_b16.ptx cuda\photon_stage_b16.cu
nvcc -ptx -O3 -arch=sm_120 -maxrregcount=128 -o cuda\build\photon_c1_schnorr.ptx cuda\photon_c1_schnorr.cu
nvcc -ptx -O3 -arch=sm_120 -maxrregcount=128 -o cuda\build\photon_c3_dual.ptx cuda\photon_c3_dual.cu
nvcc -ptx -O3 -arch=sm_120 -o cuda\build\stage_c_hash.ptx cuda\stage_c_hash.cu
```
