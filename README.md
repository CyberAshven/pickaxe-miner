# Pickaxe Miner

![Pickaxe Miner](docs/assets/pickaxe-miner-logo.png)

Rust-first, GPU-only miner for CashTokens on Bitcoin Cash.

## Scope

Pickaxe Miner first candidate is PHOTON which is live covenant and CashToken baton it is the first minable CashToken in the BCH network: the baton outpoint, NFT commitment, token amount, and PHOTON target. BCH `getblocktemplate` and `getblocktemplatelight` are not a mining job source.

Fulcrum/Electrum discovers the indexed baton. Native node RPC is used for chain validation and raw transaction broadcast.

Mainnet and Chipnet run the PHOTON v3.2 contract (mainnet category `53bd86e3f123918d2d7040449f88f7ed1bbddc309b66f2ac67cd429278f5ea58`). The retired PHOTON v0 is no longer mined; releases before v0.0.3 mine only v0.

PHOTON uses a **4% mining-work donation**: 96% of completed candidate hashes mine for your wallet. Each win pays directly. There is no additional reward split, minimum withdrawal, or deposit required. The donation is based on work, not guaranteed rewards: short sessions can have different outcomes because wins are random.

## Supported platforms

- Operating systems: Windows x86_64 and Linux x86_64
- NVIDIA: CUDA PTX `sm_120`
- AMD and Intel: GPUs mine through the portable T2 engine on Vulkan, selected automatically. A discrete GPU always comes first; an integrated GPU is used automatically only when no discrete GPU is present, or when chosen with `--device`. AMD HIP `gfx1036` remains available with `--backend hip`.
- Apple Silicon (Metal) and browser (WebGPU/WASM) builds: [portable build instructions](docs/portable.md).

## Quick start: mainnet TUI

From the extracted download folder, run the command for your platform:

| Platform | Command |
| --- | --- |
| Windows PowerShell | `.\pickaxe.exe mine --network mainnet` |
| Linux | `./pickaxe mine --network mainnet` |
| Apple Silicon Mac | `./pickaxe mine --network mainnet --backend wgpu` |

The interactive setup opens with Mainnet selected. Enter your payout address
and choose **Start mining**. Saved profiles keep your settings for next time.
To skip setup and start directly in the mining TUI, append
`--address "YOUR_MAINNET_PAYOUT_ADDRESS"`, replacing the placeholder with your
valid payout CashAddr. Servers are selected automatically.

## Install

Download the archive for your operating system from the [v0.0.3 release](https://github.com/CyberAshven/pickaxe-miner/releases/tag/pickaxe-miner-v0.0.3). The release includes `SHA256SUMS.txt` to verify your download. Extract the archive and start mining:

Linux:

```bash
tar -xzf pickaxe-miner-v0.0.3-linux-x86_64.tar.gz
cd pickaxe-miner-v0.0.3-linux-x86_64
./pickaxe mine
```

Windows PowerShell:

```powershell
Expand-Archive pickaxe-miner-v0.0.3-windows-x86_64.zip -DestinationPath pickaxe-miner-v0.0.3-windows-x86_64
cd pickaxe-miner-v0.0.3-windows-x86_64
.\pickaxe.exe mine
```

Headless, from the same directory:

```bash
./pickaxe mine --no-tui
```

```powershell
.\pickaxe.exe mine --no-tui
```

## Setup and profiles

Without `--no-tui`, the miner opens a short setup:

1. **Profiles**: your saved profiles. Enter on one opens its settings with Start selected, so a second Enter mines. `R` renames and `D` deletes (with a confirmation).
2. **Hardware**: GPU or ASIC.
3. **Network**: Mainnet or Chipnet.
4. **Token**: the GPU tokens for that network, or for ASIC, "BCH + all merge-mined tokens" or an ASIC-exclusive token. ASIC mining is not supported yet.
5. **Settings + Start**: GPU, payout address, intensity, Fulcrum servers, BCH node and profile name on one page.

Fulcrum servers and nodes you add are saved once per network in `config.sources.json`, next to `config.profiles.json`, and shared by every profile on that network. Servers you add that are not built in are tried first; built-in servers, including ones you also saved, are ranked by health. Servers and nodes saved inside older profiles move there automatically. Press `S` while mining to change the address or intensity, or to use another server for the session.

## Chipnet PHOTON test

Use v0.0.3 or newer, which includes Chipnet support. After building this branch, run from the project root with the GPU files in place:

```bash
./target/release/pickaxe_miner mine --chipnet --backend cuda --address 'bchtest:YOUR_TOKEN_AWARE_P2PKH_ADDRESS'
```

Replace the quoted address with your valid Chipnet P2PKH or token-aware P2PKH payout CashAddr. Mainnet requires `bitcoincash:` addresses and Chipnet requires `bchtest:` addresses; mismatched networks are rejected, never converted. It uses the Chipnet PHOTON contract and Chipnet Fulcrum endpoints; mainnet remains the default without `--chipnet`.

New Chipnet wins pay the selected wallet directly, using the same donation policy. No local reward wallet or payout batching is created for new mining.

The earlier Chipnet batch-payout preview is no longer supported. Rewards it held under `chipnet-funding.key` are Chipnet test coins and are not recovered; you can delete that file. If the preview left a pending `pending-reward-chipnet.json`, the miner says so and asks you to delete it.

In the Chipnet TUI, `now` is the recent effective rate, `active GPU` is the last completed GPU batch rate, and `wall avg` averages candidates over all elapsed time, including reward handling pauses. A paused miner shows `now` as zero while retaining the last `active GPU` rate.

After a verified direct win, the GPU mines the baton that win creates while the claim is broadcast, instead of waiting for Fulcrum to list the unconfirmed claim. A new claim is broadcast before any further state check, and also sent in the background to up to two other configured Fulcrum servers so it reaches more of the network at once. If the claim turns out stale, or another transaction already spent its baton, that work is discarded and mining continues on the baton Fulcrum reports. A direct claim pays its reward inside the claim transaction, so if a claim recorded as stale still confirms, the recipient is paid.

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

The binary is `target/release/pickaxe_miner` (`pickaxe_miner.exe` on Windows). `--version` prints `pickaxe 0.0.3`. Keep the supplied `cuda/build` directory beside the executable. The release archives include the required GPU files.

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

## Development

`dev` is the alpha/testing branch; `master` is stable. Changes move through reviewed PRs. See [development and release steps](docs/development.md) and [Rust security checks](docs/rust-security.md).

## License

Pickaxe Miner is licensed under the [GNU Affero General Public License, version 3.0 only](LICENSE) (`AGPL-3.0-only`). Third-party components and reference materials retain their respective licenses.
