#!/usr/bin/env bash
# #### PR #42: mmpOS reads one JSON line ($1 is the GPU count, $2 the log).
cd "$(dirname "$0")" || exit 1
./pickaxe farm-os stats --os mmpos --config "$PWD/pickaxe.json" 2>/dev/null ||
  echo "Miner API connection failed"
