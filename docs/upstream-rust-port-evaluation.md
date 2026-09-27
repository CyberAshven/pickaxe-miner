# Upstream-derived Rust backend experiment

This branch consumes `ufsecp-core` from the UltrafastSecp256k1 fork at
`e346efe3239175ef5d8dc97ca6e46920658bdf97`. The crate is a direct MIT port of
upstream public field/scalar/point arithmetic for portable CPU and NVIDIA GPU.
It is distinct from the independent Pickaxe Rust engine in `rust-engine/src`.
Both implementations remain available; neither changes the default miner.

The port's source, independent oracle tests, provenance and scope live in
[`rust/ufsecp-core`](https://github.com/CyberAshven/UltrafastSecp256k1/tree/e346efe3239175ef5d8dc97ca6e46920658bdf97/rust/ufsecp-core).
It uses Rust with inline PTX for the original GPU carry chains, without a C++
runtime. This is a subset of the library, not a Rust rewrite of every upstream
signature protocol, CT engine, GLV path or hardware backend.

Pickaxe keeps transaction hashing, nonce control, GPU scheduling and its
specialized fixed-key Montgomery multiplication. CPU reward signing still
uses RustCrypto k256: the new variable-time public backend must not process
funded wallet keys. The upstream generic Barrett scalar implementation is
included and tested on CPU and GPU even though the miner retains its faster
fixed-key specialization.

## Reproduce

```powershell
./tools/build-rust-kernels.ps1 -Upstream -Architecture sm_120
cargo build --locked --release --features upstream-rust-engine
```

The default `build-rust-kernels.ps1` command still builds the independent
Pickaxe Rust engine. Both emit `cuda/build/photon_rust.ptx`; preserve each
artifact and its hash when comparing them. Build for the target GPU, and
distribute the exact tested PTX with the matching experimental executable.
Do not infer which engine a previously built PTX contains from its filename.

For the established offline mining oracle and matched comparison, build the
host test executable with `--features incremental-k` (without `rust-engine`).
Stop the one live TUI normally, run tests exclusively, and restore exactly one
TUI afterward:

```powershell
cargo test --locked --release --features incremental-k incremental_k_rust_correctness -- --ignored --nocapture --test-threads=1
node tools/reward-policy-vm/photon-layout.mjs artifacts/incremental-k/rust-vectors.json
cargo test --locked --release --features incremental-k incremental_k_rust_comparison -- --ignored --nocapture --test-threads=1
```

The upstream crate README contains the separate raw GPU arithmetic probe.
Tests use synthetic keys and do not broadcast transactions.

## Initial results

On the local RTX 5070 Ti Laptop GPU, driver 616.92, the port passed 13,920
independently reconstructed mining candidates and 216 BCH 2026 VM cases
(103 accepted / 113 rejected, checked under standard policy and consensus).
The separate upstream GPU probe passed 773 boundary/random vectors, including
generic scalar multiplication/inversion and full-width generator multiplication.
Host debug/release tests checked 1,539 field and 1,539 scalar vectors against
BigUint and public points against libsecp256k1.

| Matched session | Baseline MH/s | Port MH/s | Change |
|---|---:|---:|---:|
| Original Pickaxe | 114.5414 | 122.0914 | +6.59% |
| Native upstream at `5536321b` | 114.7347 | 119.7729 | +4.39% |

Each comparison uses eight alternating 12-second trials after 45 seconds
warmup, with one second settling, 565,248 candidates/batch, 32 points/walk lane
and 16 inversions/batch lane. Aggregate rates use total candidates / total
elapsed time. No compilation or second miner runs during timing. All trials,
including clock fluctuations, are retained. These are end-to-end mining rates;
they are not isolated library benchmarks or proof of performance on other GPUs.

## Pinned package confirmation

| Final matched session | Baseline MH/s | Port MH/s | Change |
|---|---:|---:|---:|
| Original Pickaxe | 114.2391 | 123.1706 | +7.82% |
| Native upstream at `5536321b` | 115.2049 | 120.4858 | +4.58% |

[Every timed trial and its metadata](upstream-rust-port-results.json) is retained,
including clock fluctuations. The pinned PTX SHA-256 is
`b2081dc6a02bf0db86d83fd3b86879901087157ef6ef92ca1f92a449327247da`.
Both final sessions used this same artifact. The GPU probe, mining oracle and
VM checks passed again on the pinned package. Pickaxe's all-feature suite passed
266 tests, with 21 explicit ignores and one physical wgpu test filtered; existing
CUDA full-pipeline/RFC6979 tests executed on the GPU. Formatting, lint and the
experimental release build passed locally.

The upstream Rust crate passed Windows/Linux CI and SM 7.5/12.0 compilation.
The native regression suite produced 437 passing test processes after correcting
Windows source-cwd and UTF-8 test setup. One optional OpenSSL interoperability
check remains unavailable (advisory exit 77); optional Python coincurve/noble
comparisons were unavailable. The original CTest invocation was not fully green,
and no native source/test policy was altered to hide this limitation.

Master, release assets and the deployed live binary remain unchanged. The
original single TUI is restored after exclusive tests. The fork pin is
experimental; upstream acceptance and an upstream revision pin are separate
adoption steps. This work does not establish a CPU performance improvement or
replace the existing constant-time Rust signer.
