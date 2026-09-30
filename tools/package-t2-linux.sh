#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."
version="$(awk -F '"' '/^version = / { print $2; exit }' Cargo.toml)"
package="pickaxe-miner-v${version}-t2-gpu-linux-x86_64"
binary="${CARGO_TARGET_DIR:-target}/release/pickaxe_miner"
test -x "$binary"
test "$("$binary" --version)" = "pickaxe ${version}"

staging="$(mktemp -d)"
trap 'rm -rf "$staging"' EXIT
mkdir -p "$staging/$package/cuda/build" dist
install -m 0755 "$binary" "$staging/$package/pickaxe"

for artifact in \
  photon_rust.ptx \
  stage_a_rfc6979.ptx \
  photon_stage_b16.ptx \
  photon_c1_schnorr.ptx \
  photon_c3_dual.ptx \
  photon_t2_tail.ptx; do
  test -s "cuda/build/$artifact"
  grep -q '^\.target sm_120$' "cuda/build/$artifact"
  cp "cuda/build/$artifact" "$staging/$package/cuda/build/"
done

cp README.md "$staging/$package/"
for license in LICENSE LICENSE-*; do
  if [[ -f "$license" ]]; then cp "$license" "$staging/$package/"; fi
done
cat > "$staging/$package/BACKENDS.txt" <<'EOF'
Pickaxe Miner for Linux x86_64 and NVIDIA sm_120 GPUs.
CUDA PTX targets sm_120 GPUs.
The default CUDA T2 backend is Rust. This package contains the kernels it loads.
HIP and wgpu are not validated or packaged in this local archive.
No CPU mining fallback is available.
Set a payout address with --address; no address is built into the binary.
EOF

tar -C "$staging" -czf "dist/$package.tar.gz" "$package"
(cd dist && sha256sum "$package.tar.gz" > "$package.SHA256SUMS.txt")
ls -lh "dist/$package.tar.gz" "dist/$package.SHA256SUMS.txt"
