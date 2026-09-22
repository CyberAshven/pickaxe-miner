# Pickaxe Miner — Definition of Done (locked 2026-09-21)

GPU-only product. No CPU mining fallback.

## Platforms
- Windows + NVIDIA (primary: RTX 5070 Ti / Blackwell / sm_120)
- Windows + AMD
- Linux + NVIDIA
- Linux + AMD

## Backends
`--backend auto|cuda|hip|wgpu` (default auto). CUDA and HIP are the validated
production mining engines. WGPU hardware adapter discovery is part of the CLI
surface and must exclude CPU/software adapters. Production `--backend wgpu`
mining remains fail-closed until the authoritative PHOTON WGSL path is wired,
verified against the reference vectors, and validated on supported hardware.
That validated WGPU mining path remains a Definition-of-Done blocker.

## Intensity
10–100% (default 100). Must scale **real GPU work** live. Pause separate (Space/P ≠ 10%).

## UI
Ratatui + Crossterm product TUI. Commands: devices, mine, benchmark, etc.

## Success
Operator mining live end-to-end, committed/pushed to CyberAshven/pickaxe-miner.

## Owners
- Lead Dev: native CUDA/HIP search, WGPU portability path, backends, intensity, devices
- Dev Assist: Electrum/node, win-tx, arm/applysig/broadcast

## Mining source (locked 2026-09-21)
PHOTON jobs are derived from the live covenant/CashToken mutable baton state:
baton outpoint and height, NFT commitment, current height, PHOTON target, token
amount, and reward. Fulcrum/Electrum is the current indexed baton-discovery path.
If a node-native indexed path is added, it must recover and validate the same
PHOTON state.

Native node RPC remains authoritative for chain validation and raw transaction
broadcast. BCH `getblocktemplatelight` / `getblocktemplate` data must never be
substituted for a PHOTON baton or target.
