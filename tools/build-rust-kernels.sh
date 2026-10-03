#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
architecture=${1:-sm_120}
if [[ ! $architecture =~ ^sm_[0-9]+$ ]]; then
  echo 'Expected an SM architecture such as sm_120' >&2
  exit 2
fi

ptx_target_dir=${PICKAXE_RUST_PTX_TARGET_DIR:-"$repo_dir/artifacts/rust-engine/target"}
export CARGO_TARGET_DIR=$ptx_target_dir
export RUSTFLAGS="-C target-cpu=$architecture -C panic=abort"

cargo +nightly-2026-04-02 build \
  --locked --release --features upstream-rust \
  --manifest-path "$repo_dir/rust-engine/Cargo.toml" \
  --target nvptx64-nvidia-cuda -Z build-std=core

install -D -m 0644 \
  "$ptx_target_dir/nvptx64-nvidia-cuda/release/pickaxe_rust_engine.ptx" \
  "$repo_dir/cuda/build/photon_rust.ptx"
