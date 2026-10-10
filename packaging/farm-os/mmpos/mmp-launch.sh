#!/usr/bin/env bash
# #### PR #42: mmpOS passes --coin, --pool HOST:PORT, --user WALLET.WORKER,
# --password, --api-port and the extra flags; `pickaxe farm-os mine` maps
# them (src/farm_os.rs). Server "solo", port 1, mines without a pool.
cd "$(dirname "$0")" || exit 1
exec ./pickaxe farm-os mine --config "$PWD/pickaxe.json" "$@"
