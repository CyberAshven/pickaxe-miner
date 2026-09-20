# Pickaxe reference library

Local mirrors / cites for protocol work. **Not** product code.

## PHOTON (postcorps miner / contract)

- Live miner UI (reference only): https://photon.postcorps.com/
- Mirrored here: `reference/index.html`, `miner.js`, `photon-miner.wgsl`, `test-vectors.js`, `generator-table.bin`
- Extracted constants: `photon_*.hex`, `SOURCE.md`
- Product DoD / mining contract: `docs/DEFINITION_OF_DONE.md`
- Codex PERFORMANCE CONTRACT (private bridge): `.agent-bridge/user-directive.md` (not for public git)

PHOTON is the PoW / CashToken baton mining path Pickaxe implements.

## vox (2qx) — Unspent v3 / DeFi building blocks

- Public: https://github.com/2qx/vox
- License: Unlicense
- Local clone: `reference/vox/`
- Description: collection of small BCH decentralized finance apps (CatDex, Dutch, Flash, Locktime, Subscription, Vox chat, …) using Unspent v3 NFT protocol records.

Use as **prior art / covenant patterns** for BCH CashToken + P2S design — not a substitute for the PHOTON mining contract.

## Hygiene

- Do not commit secrets, Chipnet keys, or private bridge text into public files.
- Prefer citing public URLs when discussing vox/PHOTON outside this tree.
