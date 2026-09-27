# Pickaxe Miner

![Pickaxe Miner](docs/assets/pickaxe-miner-logo.png)

Rust-first, GPU-only miner for CashTokens on Bitcoin Cash.

## Scope

Pickaxe Miner first candidate is PHOTON which is live covenant and CashToken baton it is the first minable CashToken in the BCH network: the baton outpoint, NFT commitment, token amount, and PHOTON target. BCH `getblocktemplate` and `getblocktemplatelight` are not a mining job source.

Fulcrum/Electrum discovers the indexed baton. Native node RPC is used for chain validation and raw transaction broadcast.

## Supported platforms

- Operating systems: Windows x86_64 and Linux x86_64
- NVIDIA: CUDA PTX `sm_120`
- AMD: HIP `gfx1036`
- No ARM, ARM64, or macOS build

## Install

Download the archive for your operating system from the [v0.0.1 release](https://github.com/CyberAshven/pickaxe-miner/releases/tag/pickaxe-miner-v0.0.1). The release includes `SHA256SUMS.txt` to verify your download. Extract the archive and start mining:

Linux:

```bash
tar -xzf pickaxe-miner-v0.0.1-linux-x86_64.tar.gz
cd pickaxe-miner-v0.0.1-linux-x86_64
./pickaxe mine
```

Windows PowerShell:

```powershell
Expand-Archive pickaxe-miner-v0.0.1-windows-x86_64.zip -DestinationPath pickaxe-miner-v0.0.1-windows-x86_64
cd pickaxe-miner-v0.0.1-windows-x86_64
.\pickaxe.exe mine
```

Headless, from the same directory:

```bash
./pickaxe mine --no-tui
```

```powershell
.\pickaxe.exe mine --no-tui
```

## Diagnostic logging

Optional file logging is disabled by default. Set `PICKAXE_TUI_LOG` before starting the interactive TUI to append timestamped events and status snapshots (about every 10 seconds) to a local file. Choose a writable location; its parent directory must already exist. This option does not apply to `--no-tui` mode.

Windows PowerShell, from the extracted release directory:

```powershell
$env:PICKAXE_TUI_LOG = "$PWD\pickaxe-diagnostic.log"
.\pickaxe.exe mine
```

To disable logging for the next launch in that PowerShell session:

```powershell
Remove-Item Env:PICKAXE_TUI_LOG -ErrorAction SilentlyContinue
.\pickaxe.exe mine
```

Linux, from the extracted release directory:

```bash
PICKAXE_TUI_LOG="$PWD/pickaxe-diagnostic.log" ./pickaxe mine
```

That Linux command enables logging for that invocation only. If the variable was exported in your shell, remove it before the next launch:

```bash
unset PICKAXE_TUI_LOG
./pickaxe mine
```

Stop the existing miner before relaunching. Changing the variable in a shell does not change a running miner, and a launcher script that sets it will enable logging again. There is no live logging toggle; `/logs` only displays runtime events in the TUI.

Log files are appended to without automatic rotation or a size limit. After an investigation, stop the miner before archiving or deleting the log, and disable logging for subsequent runs.

## Build

```bash
rustup toolchain install nightly-2026-04-02 --profile minimal --component rust-src,llvm-tools-preview,llvm-bitcode-linker
pwsh -File tools/build-rust-kernels.ps1 -Architecture sm_120
cargo build --release --locked
```

The cargo binary is `target/release/pickaxe_miner` (`pickaxe_miner.exe` on Windows). `--version` prints `pickaxe 0.0.1`.

The default CUDA backend uses the pinned Rust arithmetic port of UltrafastSecp256k1. Building its kernels requires PowerShell 7 (`pwsh`), including on Linux. Keep `cuda/build`, including `photon_rust.ptx`, beside the executable when distributing it. HIP retains its existing backend.

The example targets SM 12.0, as do the release archives; use your GPU's SM architecture when building for other NVIDIA devices. The pinned engine and compiler make comparisons repeatable, but MH/s also depends on the GPU, driver, clocks, power limits and cooling. Physical validation currently covers SM 12.0; SM 7.5 has compilation coverage.

For the legacy native CUDA backend, build with `--no-default-features --features incremental-k`. Historical source is preserved in [legacy-engine-by-cyberashven](reference/legacy-engine-by-cyberashven/README.md).

Contract identity is maintained in one [protocol profile](docs/protocol-profile.md); changing it requires rebuilding and end-to-end validation.

Interactive TUI:

```bash
cargo run --release -- mine
```

Headless:

```bash
cargo run --release -- mine --no-tui
```

List GPUs:

```bash
cargo run --release -- devices
```

Benchmark:

```bash
cargo run --release -- benchmark
```
## Note
- `wgpu` is not a verified production backend. Production mining is CUDA or HIP.
  
## Limitations

- There is no CPU mining fallback.
- CUDA performance was measured on the local NVIDIA GPU; HIP artifacts are build-verified, without a physical AMD performance claim.

## Features

- High-performance native GPU mining
- Live PHOTON CashToken baton discovery
- Automatic winner verification and submission
- Runtime intensity control from 10% to 100%
- Safe live payout-address switching
- Fulcrum/Electrum connectivity
- Native BCH node validation and transaction broadcast
- Interactive Ratatui interface
- Headless and JSON operation
- Persistent, bounded GPU memory architecture
- Stale-job and generation protection
- Device discovery and benchmark tools

The `reference/` directory contains PHOTON reference material used for implementation and correctness testing.

## Development

`dev` is the alpha/testing branch; `master` is stable. Changes move through reviewed PRs. See [development and release steps](docs/development.md) and [Rust security checks](docs/rust-security.md).

## License

Pickaxe Miner is licensed under the [GNU Affero General Public License, version 3.0 only](LICENSE) (`AGPL-3.0-only`). Third-party components and reference materials retain their respective licenses.
