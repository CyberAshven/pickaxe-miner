# Official Pickaxe donation policy (Bandar 2026-09-21)

- Donation is exactly **2%** of the PHOTON mining reward.
- Split from the mining reward on the win path — **NOT** taken later from the miner wallet/address/balance.
- Donation address is **compile-time hard-coded** in the official Pickaxe release.
- Users **cannot** change, disable, override, or redirect it via CLI, config, env, Ratatui, local API, endpoint, payout-address change, or runtime command.
- Miner payout address remains user-changeable and receives the remaining **98%**.
- Changing miner payout **MUST NOT** affect donation address or percentage.
- Official donation address may change **ONLY** by changing source and publishing a **NEW** Pickaxe release.
- Existing releases keep the address they were compiled with.
- Forks may change/remove this in their own fork; acceptable; not cryptographically prevented.
- **Do NOT** modify the PHOTON protocol/covenant to enforce this.
- Preserve original PHOTON compatibility. If a 3-output 98/2 tx is covenant-invalid, replace with a **protocol-valid** reward-splitting design — do not change PHOTON.

Hard-coded address (current): `bitcoincash:qqn3aqnrarpvecss9vned5v9693j9p37w5pmzz4mn3`
