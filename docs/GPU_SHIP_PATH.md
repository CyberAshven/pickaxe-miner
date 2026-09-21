# Pickaxe GPU ship path (locked 2026-09-20)

Researchy brief + CoS product lock.

## Ship path (Windows RTX)

1. **CUDA first** — `cudarc` host + `.cu`/PTX kernels (peak hashrate on NVIDIA).
2. **Native AMD path** — HIP/ROCm where validated, otherwise an appropriate native GPU API.
3. **FPGA** — later optional; do not block GPU ship.

wgpu/WebGPU is detached from the production backend stack. The authoritative
WGSL implementation remains useful for correctness comparison and portability
research, but the production binary does not select or fall back to it.

## Sequence after Electrum/job

1. CPU oracle matching postcorps Schnorr/HASH256 vectors
2. CUDA Stage A (message SHA + RFC6979-ish / midstate)
3. CUDA Stage B fixed-base kG (16-bit ~64 MiB table; split-B early if regs spill)
4. CUDA Stage C Schnorr/HASH256 tail; end-to-end equiv gate
5. Intensity → batch (~524k ladder lessons from postcorps)
6. Multi-GPU

## Hardware note (MasterChief)

- GPU: NVIDIA GeForce RTX 5070 (seen via `nvidia-smi`)
- Driver reports CUDA 13.2
- **Blocker:** CUDA Toolkit / `nvcc` not on PATH as of 2026-09-20 — install toolkit before kernel builds

## Prior art

- https://photon.postcorps.com/ (detached WebGPU correctness/performance reference, M24–M67 kernel design)
- https://github.com/2qx/UltrafastSecp256k1 (bch-photon CUDA worker branch — prior art only)
- https://github.com/shrec/UltrafastSecp256k1
- https://docs.rs/cudarc/latest/cudarc/
