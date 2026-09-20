# CUDA kernels (Pickaxe)

## Stage A — HASH256
- Source: `stage_a_hash256.cu`
- PTX: `build/stage_a_hash256.ptx` (`sm_120`)
- Launcher: `src/cuda_stage_a.rs`
- Status: GPU == host verified on RTX 5070 Ti

## Stage B — k*G (secp256k1)
- Source: `stage_b_kg.cu` (Jacobian double-and-add, 8×u32 limbs, MSVC-safe)
- PTX: `build/stage_b_kg.ptx` (`sm_120`, `-maxrregcount=128`, 1-thread blocks for register budget)
- Launcher: `src/cuda_stage_b.rs`
- Status: GPU == host (`secp256k1`) verified on RTX 5070 Ti
- Next: 16-bit fixed-base table for hashrate; Stage C Schnorr/HASH256 tail

## Build (Windows)
```bat
call "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat"
nvcc -ptx -O3 -arch=sm_120 -maxrregcount=128 -o cuda\build\stage_b_kg.ptx cuda\stage_b_kg.cu
```
