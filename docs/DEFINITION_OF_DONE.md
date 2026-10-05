# Pickaxe Miner — Definition of Done (locked 2026-09-21)

GPU-only product. No CPU mining fallback.

## Platforms
- Windows + NVIDIA (primary: RTX 5070 Ti / Blackwell / sm_120)
- Windows + AMD
- Linux + NVIDIA
- Linux + AMD

## Backends
`--backend auto|cuda|hip|wgpu` (default auto). CUDA, HIP and the portable
WGPU engine mine in production. WGPU hardware adapter discovery must exclude
CPU/software adapters. The WGPU engine runs the stages generated from the
shared Rust engine; its T2 end-to-end oracle checks every candidate against an
independent CPU reconstruction on Vulkan, DirectX 12 and browser WebGPU (see
[GPU code map](gpu-sources.md)). Still to validate on hardware: Intel GPUs,
Linux GPUs, Apple Silicon and discrete AMD GPUs.

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
