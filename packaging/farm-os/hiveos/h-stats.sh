#!/usr/bin/env bash
# #### PR #42: the HiveOS agent sources this for $khs (kH/s) and $stats
# (JSON); it must never exit. The miner's status file gives both.
# shellcheck disable=SC2034 # khs and stats are read by the agent.
pickaxe_dir=$(dirname "${BASH_SOURCE[0]}")
mapfile -t pickaxe_lines < <("$pickaxe_dir/pickaxe" farm-os stats --os hiveos \
  --config "$pickaxe_dir/pickaxe.json" 2>/dev/null)
khs=${pickaxe_lines[0]:-0}
stats=${pickaxe_lines[1]:-'{"hs":[],"hs_units":"hs","uptime":0}'}
