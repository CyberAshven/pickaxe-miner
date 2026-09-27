#!/usr/bin/env bash
# Rebuild CUDA PTX artifacts for the selected GPU architecture.
set -euo pipefail
cd "$(dirname "$0")/.."
NVCC="${NVCC:-nvcc}"
OUT=cuda/build
ptx() { "$NVCC" -ptx -O3 "$@"; }

ptx -arch=sm_75 -o $OUT/stage_a_hash256.ptx cuda/stage_a_hash256.cu
for K in stage_a_mine stage_a_rfc6979 stage_c_hash; do
  ptx -arch=sm_120 -o $OUT/$K.ptx cuda/$K.cu
done
for K in stage_b_kg photon_stage_b16 photon_c1_schnorr photon_c3_dual photon_incremental_k photon_t2_tail photon_t2_value; do
  ptx -arch=sm_120 -maxrregcount=128 -o $OUT/$K.ptx cuda/$K.cu
done
ptx -arch=sm_120 -maxrregcount=128 -DPICKAXE_INCREMENTAL_K_EXPERIMENT -o $OUT/photon_incremental_c3.ptx cuda/photon_c3_dual.cu
# Arithmetic oracle only; not loaded by the miner.
ptx -arch=sm_120 -maxrregcount=128 -DPICKAXE_C1_SCALAR_CHECK -o $OUT/scalar-check.ptx cuda/photon_c1_schnorr.cu
if [ -n "${PTX_ISA:-}" ]; then
  sed -i "s/^\.version .*/.version $PTX_ISA/" $OUT/*.ptx
fi
grep -H '^\.version' $OUT/*.ptx
