#!/usr/bin/env bash
# #### PR #42: HiveOS starts the miner with this. `pickaxe farm-os mine` maps
# the flight sheet's fields to Pickaxe's own flags (src/farm_os.rs).
# shellcheck disable=SC2154 # CUSTOM_* come from h-manifest.conf.
set -o pipefail
cd "$(dirname "$0")" || exit 1
# shellcheck source=/dev/null
. ./h-manifest.conf
mkdir -p "$(dirname "$CUSTOM_LOG_BASENAME")"
mapfile -d '' -t fields < "$CUSTOM_CONFIG_FILENAME"
extra=$(< "$CUSTOM_CONFIG_FILENAME.extra")
# The extra flags split on spaces; wildcards are never expanded.
set -f
# shellcheck disable=SC2086
./pickaxe farm-os mine --config "$PWD/pickaxe.json" "${fields[@]}" -- $extra 2>&1 |
  tee -a "$CUSTOM_LOG_BASENAME.log"
