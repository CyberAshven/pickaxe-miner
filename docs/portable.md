# Portable builds

The macOS Apple Silicon command-line miner and the browser WebGPU miner build
from the same Rust crate as Windows and Linux. They share transaction layouts,
protocol rules, signatures, independent winner checks, dynamic fee calculations
and the token's donation policy. The browser uses the same WGSL backend that
the native portable build runs through Metal on macOS or Vulkan elsewhere.

Backend performance depends on the GPU and driver; Metal and WebGPU do not
promise CUDA T2 performance.

## Apple Silicon

The **Portable builds** workflow creates a `pickaxe-miner-v0.0.3-macos-arm64.tar.gz`
archive on each pull request and master/dev update. Extract it, verify its
checksum and source commit, then run:

```sh
./pickaxe mine --network mainnet --backend wgpu
```

This opens the mainnet TUI setup. Enter your payout address and choose
**Start mining**; connection settings are automatic. To open the mining TUI
and start immediately with a known address, run:

```sh
./pickaxe mine --network mainnet --backend wgpu --address "YOUR_MAINNET_PAYOUT_ADDRESS"
```

Replace the placeholder with your valid payout CashAddr. Use `--network chipnet`
instead of `--network mainnet` for test mining.

This is a command-line archive, without Developer ID signing or notarization.
No Apple Developer account is needed to build it. macOS may require explicit
approval to open a downloaded executable. Do not disable system-wide security
settings. The normal setup, profiles and native runtime are shared with Linux
and Windows.

Build on an Apple Silicon Mac:

```sh
cargo build --locked --release --no-default-features --features portable-wgpu
./target/release/pickaxe_miner mine --backend wgpu
```

## Browser

The same workflow creates `pickaxe-web-experimental.tar.gz`, a static bundle
with the compiled Rust WASM, JavaScript bindings, page and verified GPU table.
Anyone can host its `web/` directory over HTTPS. No Pickaxe-operated website,
pool, account service or backend server is required. Browsers still download
the application and its approximately 64 MiB lookup table.

To serve an extracted bundle locally with Python installed:

```sh
python -m http.server 8080 --bind 127.0.0.1 --directory web
```

Open `http://127.0.0.1:8080`. Choose the network and enter a payout address;
mining starts only when you press **Start mining**. No wallet private key is
requested. Use a WebGPU-capable browser on HTTPS or localhost. Keep the tab
open: browsers may throttle background tabs or suspend work on sleep.

Close desktop miners before starting. Web Locks prevent two mining tabs on
the **same website**; browser isolation cannot enforce the desktop process
lock or coordinate unrelated websites.

Browser work uses the compiled token policy and direct payouts. Payout address
and pending transaction bytes are stored locally. An interrupted submission is
resolved before new mining; no funded reward key is stored. The experimental
browser currently uses Fulcrum WSS for jobs and broadcasts, with bounded
connection retries. Native-node RPC and the native TUI remain desktop features.

Build the browser bundle (Rust, Python 3.11+, Clang and Node.js 24):

```sh
rustup target add wasm32-unknown-unknown
cargo install --locked wasm-bindgen-cli --version 0.2.128
python tools/build-browser.py
node --test web/rpc.test.mjs web/submission.test.mjs
node web/smoke.mjs
python -m http.server 8080 --bind 127.0.0.1 --directory dist/web
```

Use the wasm-bindgen CLI version in `Cargo.lock`; the build script checks it.
Windows also needs Clang on PATH. Browser JavaScript contains platform I/O
and presentation, not a second implementation of the mining protocol.

## Validation and distribution

Existing native CI remains in place. Portable CI additionally builds ARM64
macOS and WASM, validates the generated WASM API and browser transport, and
uploads archives with source commit and SHA-256 checksums. The shared WGSL is
validated without a GPU in the Rust suite. Hardware checks are deliberately
separate from those compile checks. Apple Silicon passes the ARM64 build and
CPU checks in CI; live mining on a physical Mac has not yet been tested.

This workflow does not create tags or publish releases. Package version remains
0.0.3. The Mac archive can be attached to the existing release after merge;
tested Windows/Linux downloads do not need replacement for this change.

Source is available at <https://github.com/CyberAshven/pickaxe-miner>. Distributors
must provide the corresponding source under AGPL-3.0; modified hosts must offer
their modified source too. The bundle records its source commit.
