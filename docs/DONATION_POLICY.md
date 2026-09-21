# Official Pickaxe donation policy (operator 2026-09-21)

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

## Verified execution shape

- The authoritative PHOTON mining branch requires exactly **2 outputs**, so a direct 3-output 98/2 mining transaction remains disabled.
- The protocol-compatible exact split is an immediate child CashToken transaction: the PHOTON parent pays its single reward output, then the child sends 98% to the miner, 2% to the hard-coded donation address, and rolls a BCH-only sponsor reserve forward.
- `tools/reward-policy-vm/verify.mjs` executes this shape in Libauth's BCH 2026 VM using the authoritative 615-byte PHOTON transaction vector. It also checks CashToken conservation and adversarial mutations.
- The proof is deterministic and offline: it does not fund a covenant, query a node/Fulcrum endpoint, or broadcast a transaction.

Hard-coded address (current): `bitcoincash:qqn3aqnrarpvecss9vned5v9693j9p37w5pmzz4mn3`
