# Reference source

Mirrored from https://photon.postcorps.com/ on 2026-09-20 (Asia/Riyadh).

Version string: M67.38-MainContinuousWins-v1

## Included

- index.html
- miner.js
- photon-miner.wgsl
- test-vectors.js
- generator-table.bin (64 KiB fixed-base table)

## Not included

- 64 MiB M29 production table: site generates locally / OPFS; network fetch is blocked in miner.js. Skip for Stage 1.

REFERENCE ONLY. Product path is native Rust (CLI → GPU), not a web rewrite.

## vox (added 2026-09-21)

- https://github.com/2qx/vox
- Local: `reference/vox/`
- Unspent v3 DeFi apps — companion reference alongside PHOTON; see `reference/README.md`.
