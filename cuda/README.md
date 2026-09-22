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
- Semantics: exact 615-byte PHOTON transaction, nonce at bytes 390..393, signature at 426..489, strict little-endian `HASH256 < target`
- Host boundary: bounded winner count/nonces/HASH256 only; no full candidate digest readback
- Status: exact filter reused by the persistent A→B→C engine; runtime production search remains gated on live job-generation/stale-winner integration

## Persistent PHOTON A→B→C engine
- Launcher: `src/cuda_photon.rs`
- Stage A: existing exact `stage_a_rfc6979.ptx`; message SHA-256 and BCH RFC6979 stay on-device
- Stage B: `photon_stage_b16.cu` / `build/photon_stage_b16.ptx`; M45-style 4+4+4+4 fixed-base windows
- Fixed-base table: exact M67.38 M29 16-bit format, 67,108,864 bytes, generated locally and accepted only when SHA-256 is `f6238556c4cf380be0479c14511c97cf996290eed88ce220c9dd28301c87b0e1`
- C1: `photon_c1_schnorr.cu` / `build/photon_c1_schnorr.ptx`; Jacobian normalization, BCH quadratic-residue rule, challenge, fixed-private-key M39 `e*d`, and R||s all run on-device
- C2/C3: existing exact completed-transaction HASH256 and strict little-endian target filter
- Host boundary: only winner count and a configured bounded winner nonce/HASH256 array; no per-candidate message, RFC6979 scalar, point, signature, or digest readback
- Resources: one CUDA context/stream plus persistent table and candidate buffers per engine; host table bytes are dropped after the one-time upload
- Production integration: connected to immutable live job generations, CPU reconstruction/verification of returned winners, authoritative freshness recheck, durable submission journaling, and self-funded settlement. Real-mainnet natural-winner evidence remains an external validation item.

## Build (Windows)
```bat
call "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat"
nvcc -ptx -O3 -arch=sm_120 -maxrregcount=128 -o cuda\build\stage_b_kg.ptx cuda\stage_b_kg.cu
nvcc -ptx -O3 -arch=sm_120 -maxrregcount=128 -o cuda\build\photon_stage_b16.ptx cuda\photon_stage_b16.cu
nvcc -ptx -O3 -arch=sm_120 -maxrregcount=128 -o cuda\build\photon_c1_schnorr.ptx cuda\photon_c1_schnorr.cu
nvcc -ptx -O3 -arch=sm_120 -o cuda\build\stage_c_hash.ptx cuda\stage_c_hash.cu
```
