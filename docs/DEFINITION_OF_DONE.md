# Pickaxe Miner — Definition of Done (locked 2026-09-21)

GPU-only product. No CPU mining fallback.

## Platforms
- Windows + NVIDIA (primary: RTX 5070 Ti / Blackwell / sm_120)
- Windows + AMD
- Linux + NVIDIA
- Linux + AMD

## Backends
`--backend auto|cuda|hip|wgpu` (default auto). Clear fallbacks. wgpu = GPU compute only (no CPU emu).

## Intensity
10–100% (default 100). Must scale **real GPU work** live. Pause separate (Space/P ≠ 10%).

## UI
Ratatui + Crossterm product TUI. Commands: devices, mine, benchmark, etc.

## Success
Operator mining live end-to-end, committed/pushed to CyberAshven/pickaxe-miner.

## Owners
- Lead Dev: CUDA/HIP/wgpu search, backends, intensity, devices
- Dev Assist: Electrum/node, win-tx, arm/applysig/broadcast
