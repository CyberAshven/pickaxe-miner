#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
# Pin the checker; fetch fresh advisory data on every run.
cargo install --locked --version 0.20.2 cargo-deny
for manifest in Cargo.toml rust-engine/Cargo.toml; do
  cargo deny --locked --manifest-path "$manifest" --config deny.toml check advisories licenses sources bans
done
