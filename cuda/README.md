# CUDA kernels (ship path)

Toolkit confirmed on MasterChief: **CUDA 13.4** + VS 2022 Build Tools (`cl.exe`).

## Build

```bat
call "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat"
nvcc -O3 -c cuda\stage_a_hash256.cu -o cuda\build\stage_a_hash256.obj
```

## Kernels
- `stage_a_hash256.cu` — GPU HASH256(nonce_le || target32) batch (toward PHOTON Stage A)
- Stage B/C (kG / Schnorr) still TODO

Product = GPU only. `src/crypto.rs` BCH Schnorr is signing/verify gate, not a CPU miner.
