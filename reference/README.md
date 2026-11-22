# Pickaxe reference library

Local mirrors / cites for protocol work. **Not** product code.

## PHOTON (postcorps miner / contract)

- Live miner UI (reference only): https://photon.postcorps.com/
- Mirrored here: `miner.js` and `photon-miner.wgsl`, which the tests and the wgpu backend read
- Extracted constants and vectors: the `photon_*.hex` files the tests read, `photon_v32_template.json`, `SOURCE.md`
- Removed 2026-11-20 as unused (see git history): `index.html`, `test-vectors.js`, `generator-table.bin`, `photon_category.hex`, `photon_covenant_lock.hex`, `photon_script_hash.hex`, `PHOTON_CONTRACT_DoD.md`, `legacy-engine-by-cyberashven/`
- Product DoD / mining contract: `docs/DEFINITION_OF_DONE.md`

PHOTON is the PoW / CashToken baton mining path Pickaxe implements.

## vox (2qx) — Unspent v3 / DeFi building blocks

- Public: https://github.com/2qx/vox (cite it there; it is not mirrored in this tree)
- License: Unlicense
- Description: collection of small BCH decentralized finance apps (CatDex, Dutch, Flash, Locktime, Subscription, Vox chat, …) using Unspent v3 NFT protocol records.

Use as **prior art / covenant patterns** for BCH CashToken + P2S design — not a substitute for the PHOTON mining contract.

## Hygiene

- Do not commit secrets, Chipnet keys, or private bridge text into public files.
- Prefer citing public URLs when discussing vox/PHOTON outside this tree.
