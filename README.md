# Pickaxe Miner

![Pickaxe Miner](docs/assets/pickaxe-miner-logo.png)

Rust-first, GPU-only miner for CashTokens on Bitcoin Cash.

## Scope

Pickaxe Miner first candidate is PHOTON which is live covenant and CashToken baton it is the first minable CashToken in the BCH network: the baton outpoint, NFT commitment, token amount, and PHOTON target. BCH `getblocktemplate` and `getblocktemplatelight` are not a mining job source.

Fulcrum/Electrum discovers the indexed baton. Native node RPC is used for chain validation and raw transaction broadcast.

Each mined PHOTON reward gives 98% of its tokens to the miner. The existing 2% donation is split equally: 1% to the original project address (`bitcoincash:qqn3aqnrarpvecss9vned5v9693j9p37w5pmzz4mn3`) and 1% to shrec (`bitcoincash:zqqpfwsvht3uaf4y5sm53me90edmtx8cmyd0xx3fv3`).

## Supported platforms

- Operating systems: Windows x86_64 and Linux x86_64
- NVIDIA: CUDA PTX `sm_120`
- AMD: HIP `gfx1036`
- No ARM, ARM64, or macOS build

## Install

Download the archive for your operating system from the [v0.0.2 release](https://github.com/CyberAshven/pickaxe-miner/releases/tag/pickaxe-miner-v0.0.2). The release includes `SHA256SUMS.txt` to verify your download. Extract the archive and start mining:

Linux:

```bash
tar -xzf pickaxe-miner-v0.0.2-linux-x86_64.tar.gz
cd pickaxe-miner-v0.0.2-linux-x86_64
./pickaxe mine
```

Windows PowerShell:

```powershell
Expand-Archive pickaxe-miner-v0.0.2-windows-x86_64.zip -DestinationPath pickaxe-miner-v0.0.2-windows-x86_64
cd pickaxe-miner-v0.0.2-windows-x86_64
.\pickaxe.exe mine
```

Headless, from the same directory:

```bash
./pickaxe mine --no-tui
```

```powershell
.\pickaxe.exe mine --no-tui
```

## Chipnet PHOTON test

Use a build containing Chipnet support (the v0.0.2 download above predates it). After building this branch, run from the project root with the GPU files in place:

```bash
./target/release/pickaxe_miner mine --chipnet --backend cuda --address 'bitcoincash:YOUR_TOKEN_AWARE_P2PKH_ADDRESS'
```

Replace the quoted address with your valid token-aware P2PKH payout CashAddr. The miner converts a valid mainnet payout CashAddr to the equivalent `bchtest:` address with the same key hash. It uses the Chipnet PHOTON contract and Chipnet Fulcrum endpoint; mainnet remains the default without `--chipnet`.

Chipnet mining starts without an external BCH deposit. Pickaxe derives a persistent intermediate reward key from its local `chipnet-funding.key` root. Early wins pay that intermediate token address, so tokens do not reach your configured payout address on the first win. Once roughly five wins are confirmed, Pickaxe can automatically combine them and send the exact 98% / 1% / 1% token split. Higher relay fees may require more confirmed wins.

Optionally, a confirmed token-free Chipnet BCH UTXO at Pickaxe's separate funding address can enable the split immediately. Keep `chipnet-funding.key` safe until all rewards are settled: on Linux it is stored under `${XDG_STATE_HOME:-$HOME/.local/state}/pickaxe-miner/chipnet-funding.key`; on Windows, under `%LOCALAPPDATA%\Pickaxe Miner\chipnet-funding.key`.

In the Chipnet TUI, `now` is the recent effective rate, `active GPU` is the last completed GPU batch rate, and `wall avg` averages candidates over all elapsed time, including reward handling pauses. A paused miner shows `now` as zero while retaining the last `active GPU` rate.

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

Rust is the default CUDA mining backend. Build its GPU kernels with the pinned
nightly compiler, then build the executable with stable Rust.

Linux:

```bash
rustup toolchain install nightly-2026-04-02 --profile minimal --component rust-src,llvm-tools-preview,llvm-bitcode-linker
bash tools/build-rust-kernels.sh sm_120
cargo build --release --locked
```

Windows PowerShell 7:

```powershell
rustup toolchain install nightly-2026-04-02 --profile minimal --component rust-src,llvm-tools-preview,llvm-bitcode-linker
./tools/build-rust-kernels.ps1 -Architecture sm_120
cargo build --release --locked
```

The legacy CUDA C++ backend remains available with
`cargo build --release --locked --no-default-features --features tail-grind`.
Use a separate output directory when comparing the two builds.

The binary is `target/release/pickaxe_miner` (`pickaxe_miner.exe` on Windows). `--version` prints `pickaxe 0.0.2`. Keep the supplied `cuda/build` directory beside the executable. The release archives include the required GPU files.

The release CUDA files target NVIDIA `sm_120`. AMD HIP retains its existing backend. Other NVIDIA architectures require compatible CUDA files and separate validation.

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

## License

Pickaxe Miner is licensed under the [GNU Affero General Public License, version 3.0 only](LICENSE) (`AGPL-3.0-only`). Third-party components and reference materials retain their respective licenses.
