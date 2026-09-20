# CUDA kernels (ship path)

**Status:** NVIDIA driver present on MasterChief (RTX 5070, CUDA 13.2 capable) but
**CUDA Toolkit / `nvcc` not installed** — kernels cannot be compiled until the operator
installs the toolkit.

## Plan (Researchy / CoS lock)
1. Stage A — message / RFC6979-ish SHA midstates
2. Stage B — fixed-base kG (16-bit ~64MiB table; split-B if regs spill)
3. Stage C — Schnorr + HASH256 tail
4. Host via `cudarc` on Windows + Linux NVIDIA
5. wgpu fallback later for non-NVIDIA

Do not treat CPU search as the product miner.
