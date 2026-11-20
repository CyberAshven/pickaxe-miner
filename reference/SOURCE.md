# Reference source

Mirrored from https://photon.postcorps.com/ on 2026-09-20 (Asia/Riyadh).

Version string: M67.38-MainContinuousWins-v1

## Included

- miner.js
- photon-miner.wgsl

`index.html`, `test-vectors.js` and `generator-table.bin` (64 KiB fixed-base table) were mirrored too and removed on 2026-11-20 as unused; the browser package builds its own table.

## Not included

- 64 MiB M29 production table: site generates locally / OPFS; network fetch is blocked in miner.js. Skip for Stage 1.

REFERENCE ONLY. Product path is native Rust (CLI → GPU), not a web rewrite.

## vox (added 2026-09-21)

- https://github.com/2qx/vox
- No longer mirrored here (removed 2026-11-20): nothing in Pickaxe builds or tests it, and its JavaScript dependencies only added update and supply-chain surface. Cite the upstream repository and commit.
- Unspent v3 DeFi apps — companion reference alongside PHOTON; see `reference/README.md`.
