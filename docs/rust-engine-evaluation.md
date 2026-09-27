# Rust GPU engine experiment

Branch: `experiment/rust-engine`. Release/master baseline:
`59c93c17f12244a6a81419fe47df83449f2d530d`.

The requested scope is Rust GPU kernel implementation only. CPU key handling,
BCH Schnorr signing/verification, job setup, table generation and dependencies
remain unchanged from the branch base.
This is an experimental Pickaxe implementation, not a Rust wrapper around
UltrafastSecp256k1. It does not rewrite the entire upstream library or constitute
an upstream Rust-engine PR. Pickaxe's AGPL-3.0-only license continues to apply.

## Implemented

- `rust-engine/src` contains Rust field/scalar/Jacobian arithmetic and NVIDIA
  kernels for RFC6979, fixed-base multiplication, incremental point walking,
  batched inversion, dual BCH Schnorr signatures and all four PHOTON transaction
  layouts. Candidate processing and winner filtering remain on the GPU.
- `--features rust-engine` selects the Rust PTX for the CUDA engine, including
  both ordinary RFC6979 search and the existing guarded incremental search.
  It is not enabled by default. Missing Rust PTX fails rather than falling back
  to native kernels.
- Rust's built-in NVPTX target compiles these kernels. No NVCC, C++ kernel,
  or Rust-CUDA codegen plugin is used to build the Rust PTX.
  NVIDIA's driver/JIT and the operating system remain external platform APIs.
- The standalone kernel crate can also compile for the CPU to run arithmetic
  oracle tests. It is not a production CPU dependency or CPU mining backend.

## Reproduce

Install the compiler once (the application's ordinary host build stays on stable):

```powershell
rustup toolchain install nightly-2026-04-02 --profile minimal --component rust-src,llvm-tools-preview,llvm-bitcode-linker
./tools/build-rust-kernels.ps1 -Architecture sm_120
cargo build --locked --release --features rust-engine
cargo test --locked --manifest-path rust-engine/Cargo.toml
```

The architecture must match the intended GPU. Both `sm_75` and `sm_120` compiled
locally, but physical execution was validated only on the available RTX 5070 Ti
Laptop GPU (`sm_120`). Kernel compilation uses nightly 1.96.0 / LLVM 22.1.2.
CI adds separate Rust PTX compilation and arithmetic checks; remote CI has not
been run for this unpushed branch.

Stop the single live TUI normally before any GPU test. Run tests serially and
restore the original deployed TUI afterward. The following use synthetic keys
and do not broadcast transactions:

```powershell
cargo test --release --all-features -- --test-threads=1 --skip if_wgpu_present
cargo test --release --all-features incremental_k_rust_correctness -- --ignored --nocapture --test-threads=1
node tools/reward-policy-vm/photon-layout.mjs artifacts/incremental-k/rust-vectors.json
cargo test --release --features incremental-k incremental_k_rust_comparison -- --ignored --nocapture --test-threads=1
```

The A/B command deliberately omits `rust-engine`: its baseline must retain the
original kernels. The test explicitly loads the Rust candidate and rejects a
build which would silently compare Rust against itself.

## Evidence

- 591 field cases against BigUint: normalization, addition, subtraction,
  multiplication, squaring, inversion and quadratic residues.
- 260 scalar/point cases against BigUint and native libsecp256k1, including zero,
  order boundaries, infinity, doubling and inverse points.
- The authoritative PHOTON RFC6979 vector passes.
- After retaining the original CPU implementation, 265 application tests passed,
  including real CUDA tests for the Rust regular pipeline and incremental search
  supervision. Eighteen tests were ignored by
  default; the unrelated legacy WGPU hardware test was explicitly excluded.
  The suite also retains native-kernel reference tests: it is not evidence that
  HIP/WGPU were rewritten.
- The dedicated Rust incremental test passed 13,920 independently reconstructed
  candidates across eight ages, three synthetic keys, partial batches, u32
  boundaries, zero/all-pass targets, bounded readback and full-batch samples.
- All 216 accompanying BCH 2026 VM vectors agreed: 103 accepted, 113 rejected,
  under both standard and consensus validation.
- Host formatting, host Clippy with warnings denied, and the Rust-engine crate's
  formatting/Clippy checks pass. The Rust-feature release binary builds.

Explicit multiplication columns and a dedicated square implementation improved
the initial prototype. The final GPU-only experiment still fails the agreed
maximum 2% performance loss:

| Experiment | Original kernels | Rust kernels | Rust loss |
| --- | ---: | ---: | ---: |
| Initial prototype | 114.60 MH/s | 52.99 MH/s | 53.76% |
| Unrolled multiplication/squaring | 113.85 MH/s | 86.08 MH/s | 24.39% |
| Final GPU-only scope, original CPU restored | 113.96 MH/s | 85.35 MH/s | 25.11% |

Rates are total candidates divided by total measured time across each pipeline's
four trials. The first two comparisons included a CPU rewrite that has since
been reverted; only the last comparison describes the retained implementation.
Raw matched trials are saved in `rust-engine-results.json`.
These are complete pipeline measurements, not a
primitive benchmark. Each comparison uses 45 seconds of warmup, eight ABBA/ABBA
trials, one second settling and 12 seconds measured per trial, with 565,248
candidates per batch, 32 candidates per point-walk lane and 16 per C1 thread.
Power/clock/temperature values are snapshots after trials, not interval averages;
some snapshots catch idle gaps. No performance ceiling is claimed.

## Remaining adoption gates

The Rust GPU migration is **not complete across hardware backends**. HIP still
uses native kernels and the portable backend still uses WGSL. Those paths need
Rust shader implementations and their own physical-device evidence. This feature
currently establishes Rust NVIDIA kernels only. CPU cryptography is outside
the requested rewrite scope.

Performance must meet the agreed acceptance criteria before promotion. These
short tests also do not establish long-session stability or resolve the earlier
unexplained PC restart. The original deployed miner remains the live TUI, and
master/releases have not changed. Preserve the existing engine under
`reference/legacy-engine-by-cyberashven/` when an actual swap is approved; it has
not been moved during this experiment.
