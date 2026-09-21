# Pickaxe Miner

Rust-first, GPU-only miner for PHOTON on Bitcoin Cash.

- **NVIDIA:** native CUDA/PTX
- **AMD:** native HIP/ROCm
- **Platforms:** Windows and Linux
- **CPU mining fallback:** none
- **Donation:** 2%

## Features

- Native high-performance GPU mining
- Live PHOTON CashToken baton discovery
- Automatic winner verification and submission
- Runtime intensity control from 10% to 100%
- Safe live payout-address switching
- Fulcrum/Electrum connectivity with native BCH node validation and broadcast
- Interactive Ratatui interface and headless operation
- Persistent, bounded GPU memory architecture
- Stale-job and generation protection
- Benchmark and device-discovery tools

## PHOTON

PHOTON mining work is derived from the live covenant/CashToken baton state through Fulcrum/Electrum.

BCH `getblocktemplate` is not used as a PHOTON mining job source. Native BCH node RPC is used for chain validation and raw transaction broadcast.

## Build

```bash
cargo build --release
```

## Run

```bash
pickaxe mine
```

Headless:

```bash
pickaxe mine --no-tui
```

List available GPUs:

```bash
pickaxe devices
```

Benchmark:

```bash
pickaxe benchmark
```

The `reference/` directory contains PHOTON reference material used for implementation and correctness testing.
