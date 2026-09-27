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
cargo build --release --locked --features incremental-k
```

The cargo binary is `target/release/pickaxe_miner` (`pickaxe_miner.exe` on Windows). `--version` prints `pickaxe 0.0.1`.

Keep the bundled `cuda/build` directory beside the executable when distributing it. Omitting `--features incremental-k` builds the original CUDA search path.

Interactive TUI:

```bash
cargo run --release --features incremental-k -- mine
```

Headless:

```bash
cargo run --release --features incremental-k -- mine --no-tui
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

## License

Pickaxe Miner is licensed under the [GNU Affero General Public License, version 3.0 only](LICENSE) (`AGPL-3.0-only`). Third-party components and reference materials retain their respective licenses.
