# Pickaxe Miner

![Pickaxe Miner](docs/assets/pickaxe-miner-logo.png)

Rust-first, GPU-only miner for CashTokens on Bitcoin Cash.

## Scope

Pickaxe Miner first candidate is PHOTON which is live covenant and CashToken baton it is the first minable CashToken in the BCH network: the baton outpoint, NFT commitment, token amount, and PHOTON target. BCH `getblocktemplate` and `getblocktemplatelight` are not a mining job source.

Fulcrum/Electrum discovers the indexed baton. Native node RPC is used for chain validation and raw transaction broadcast.

Mainnet and Chipnet run the PHOTON v3.2 contract (mainnet category `53bd86e3f123918d2d7040449f88f7ed1bbddc309b66f2ac67cd429278f5ea58`). The retired PHOTON v0 is no longer mined; releases before v0.0.3 mine only v0.

PHOTON uses a **4% mining-work donation**: 96% of completed candidate hashes mine for your wallet. Each win pays directly. There is no additional reward split, minimum withdrawal, or deposit required. The donation is based on work, not guaranteed rewards: short sessions can have different outcomes because wins are random.

The BCH ASIC server has its own **1.5% donation** by default: 0.5% of mining
work and 1% of each block reward. Set it anywhere from 0% to 100%, in 0.5%
steps, with `stratum-v2 serve --donation <percent>` or in the dashboard's
**Advanced settings** (`a`); changes are saved. Shown percentages are rounded
up to two decimals. Other assets keep their own donation policies. Actual
rewards depend on which work wins.

## Supported platforms

Windows x86_64, Linux x86_64 and Apple Silicon macOS, plus a browser miner (WebAssembly on browser WebGPU).

| GPU | Engine | Tested |
| --- | --- | --- |
| NVIDIA GeForce RTX 50 series (`sm_120`) | CUDA | RTX 5070 Ti Laptop: about 1.48 GH/s |
| AMD Radeon RX 6000, 7000 and 9000 (`gfx1030`-`gfx1034`, `gfx1100`-`gfx1102`, `gfx1200`-`gfx1201`) | Native HIP T2 | Code objects verified; no discrete card measured yet |
| AMD integrated (Ryzen) and other AMD GPUs | Portable engine on Vulkan | Ryzen 9 9955HX3D integrated Radeon: about 20 MH/s |
| Intel GPUs | Portable engine on Vulkan, or on DirectX 12 when the Vulkan driver fails | Not yet |
| Other NVIDIA GPUs | Portable engine on Vulkan, chosen automatically (the CUDA kernels target `sm_120` only) | Not yet |
| Apple Silicon (M1 and later) | Portable engine on Metal | Built by CI; not yet run on a Mac |
| Browser miner: browsers with WebGPU (Chrome, Edge, Firefox, Safari) | Portable engine in WebAssembly on browser WebGPU | Chrome, on the integrated Radeon and the RTX 5070 Ti |

The engine is selected automatically, and one miner mines on every discrete GPU of the machine, each on its best engine ([several GPUs](#several-gpus)). An integrated GPU mines automatically only when no discrete GPU is present. `devices` lists every GPU with its engine. HIP runs C++ kernels by default; `PICKAXE_HIP_KERNELS=rust` selects the kernels built from the shared Rust engine. Which kernels each GPU runs, how they are built and how to switch: [GPU code map](docs/gpu-sources.md). All documentation: [docs](docs/README.md).

## Quick start: mainnet TUI

From the extracted download folder, run the command for your platform:

| Platform | Command |
| --- | --- |
| Windows PowerShell | `.\pickaxe.exe mine --network mainnet` |
| Linux | `./pickaxe mine --network mainnet` |
| Apple Silicon Mac | `./pickaxe mine --network mainnet --backend wgpu` |

Double-clicking the executable opens the same setup. The interactive setup opens with Mainnet selected. Enter your payout address
and choose **Start mining**. Saved profiles keep your settings for next time.
To skip setup and start directly in the mining TUI, append
`--address "YOUR_MAINNET_PAYOUT_ADDRESS"`, replacing the placeholder with your
valid payout CashAddr. Servers are selected automatically.

### Portable engine in the TUI

The portable engine mines on any GPU above that has no native engine:

| Where | Command |
| --- | --- |
| Windows | `.\pickaxe.exe mine --network mainnet --backend wgpu` |
| Linux | `./pickaxe mine --network mainnet --backend wgpu` |
| Windows on DirectX 12 | Put Microsoft's `dxcompiler.dll` and `dxil.dll` next to `pickaxe.exe` ([details](docs/portable.md#directx-12-on-windows-optional)), run `$env:PICKAXE_WGPU_API = 'dx12'`, then the Windows command |

Automatic selection already picks the portable engine for every GPU without a native engine; `--backend wgpu` forces it.

### Several GPUs

One miner drives all of a machine's GPUs: they mine the same job for your address with separate work, so they never compete for the same reward. By default every discrete GPU mines:

| Mine on | Add to the mine command |
| --- | --- |
| Every discrete GPU | nothing (default) |
| Every GPU, integrated included | `--include-integrated` |
| Chosen GPUs | `--device 0,2`, with the numbers `devices` prints |

In setup, Enter on the GPU row lists the GPUs with a checkbox each, and a profile remembers the choice. The dashboard shows each GPU's rate and temperature. A GPU that fails restarts by itself while the others keep mining.

### Browser miner

The browser miner is the same portable engine compiled to WebAssembly, mining on browser WebGPU in Chrome, Edge, Firefox or Safari. In the extracted `-browser.tar.gz` download, run this one command (Python required), then open `http://127.0.0.1:8080`, enter your payout address and press **Start mining**:

```sh
python -m http.server 8080 --bind 127.0.0.1 --directory web
```

Keep the tab open while mining. Names: `wgpu` is the Rust GPU library inside Pickaxe (the `--backend wgpu` option), and browser WebGPU is the browser's GPU API that the browser miner runs on ([names](docs/gpu-sources.md#names), [portable builds](docs/portable.md)).

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
4. **Token**: the GPU tokens for that network, or for ASIC, "BCH + all merge-mined tokens" or an ASIC-exclusive token. Choosing BCH starts the BCH ASIC server, which builds blocks from your own BCH node (see [docs/stratum-v2.md](docs/stratum-v2.md)); ASIC-exclusive tokens come later.
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
## Limitations

- There is no CPU mining fallback.
- CUDA performance was measured on the local NVIDIA GPU; HIP artifacts are build-verified, without a physical AMD performance claim.
- The portable engine (`wgpu` on Vulkan and DirectX 12, and the browser miner on browser WebGPU) was verified on an integrated Radeon and an RTX 5070 Ti. Intel GPUs, Linux GPUs and Apple Silicon have not run it yet.

## Features

- High-performance native GPU mining
- Every GPU of a machine in one miner, with per-GPU status
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

The `reference/` directory contains PHOTON reference material used for implementation and correctness testing. Its `shared-t2/` and `shared-stages/` folders are different: they hold the portable engine's WGSL, generated from `rust-engine/` by `tools/shared-gpu-proof/sync_filter.py` and never edited by hand.

## Development

`dev` is the alpha/testing branch; `master` is stable. Changes move through reviewed PRs. See [development and release steps](docs/development.md) and [Rust security checks](docs/rust-security.md).

## License

Pickaxe Miner is licensed under the [GNU Affero General Public License, version 3.0 only](LICENSE) (`AGPL-3.0-only`). Third-party components and reference materials retain their respective licenses.
