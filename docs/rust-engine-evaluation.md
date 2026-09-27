# Rust CPU and GPU engine experiment

Branch: `experiment/rust-engine`. Release/master baseline:
`59c93c17f12244a6a81419fe47df83449f2d530d`.

The branch retains both the Rust CPU rewrite and Rust NVIDIA GPU kernels for
the intended upstream contribution. CPU key handling, BCH Schnorr signing and
verification, job setup and table generation use Rust implementations.
This is an experimental Pickaxe implementation, not a Rust wrapper around
UltrafastSecp256k1. It does not rewrite the entire upstream library or constitute
an upstream Rust-engine PR. Pickaxe's AGPL-3.0-only license continues to apply.

## Implemented

- CPU public keys, BCH Schnorr signing/verification, identity creation and M29
  table generation use RustCrypto `k256` arithmetic. BCH's quadratic-residue
  signature rule is retained; this does not use the library's BIP340 signer.
  Native `secp256k1` remains a test oracle only, outside the normal dependency
  tree. CPU and GPU share the allocation-free Rust RFC6979 implementation.
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
- The standalone kernel crate also compiles for the CPU to run arithmetic
  oracle tests and provide the shared nonce routine. Production CPU signing
  uses `k256` arithmetic, not the GPU's variable-time field/point implementation.
- GPU SHA-256 uses a scalar, unrolled compression routine, fixed transaction
  block positions and fixed BCH challenge padding. Explicit feed-forward
  assignments keep the eight SHA state words in registers: the earlier
  iterator/zip form generated a local-memory loop after every compression.
  RustCrypto remains the independent SHA oracle and supplies CPU hashing/HMAC.

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
The final `sm_120` rebuild reproduces the measured PTX byte for byte; its SHA-256
is recorded in `rust-engine-results.json`.

Stop the single live TUI normally before any GPU test. Run tests serially and
restore the original deployed TUI afterward. The following use synthetic keys
and do not broadcast transactions:

```powershell
cargo test --release --all-features -- --test-threads=1 --skip if_wgpu_present
cargo test --release --all-features rust_generator_table_matches_authoritative_checksum -- --ignored --test-threads=1
cargo test --release --all-features incremental_k_rust_correctness -- --ignored --nocapture --test-threads=1
node tools/reward-policy-vm/photon-layout.mjs artifacts/incremental-k/rust-vectors.json
cargo test --release --features incremental-k incremental_k_rust_comparison -- --ignored --nocapture --test-threads=1
cargo test --release --features incremental-k incremental_k_rust_ultrafast_comparison -- --ignored --nocapture --test-threads=1
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
- 256 SHA cases compare fixed 97-byte challenges, compression from arbitrary
  states and second-hash padding against RustCrypto.
- 128 CPU BCH Schnorr signatures match independent native points and BigUint
  scalars; verification rejects mutated signatures and noncanonical scalars.
- The Rust-generated 64 MiB M29 table matches its authoritative SHA-256.
- On the final hashing candidate, 266 application tests passed, including real
  CUDA tests for the Rust regular pipeline and incremental search supervision.
  Twenty-one benchmarks and explicit oracle tests were ignored by
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

The Rust contribution must preserve performance against the applicable existing
implementations. A repeatable improvement is required before presenting it as a
performance upgrade. The earlier acceptance of approximately 2% loss concerned
adopting the maintained native library, not submitting a slower Rust rewrite.

Each row is a separate matched session. Loss is negative when Rust is faster.

| Experiment | Original kernels | Rust kernels | Rust loss |
| --- | ---: | ---: | ---: |
| Initial prototype | 114.60 MH/s | 52.99 MH/s | 53.76% |
| Unrolled multiplication/squaring | 113.85 MH/s | 86.08 MH/s | 24.39% |
| GPU-only comparison, original CPU | 113.96 MH/s | 85.35 MH/s | 25.11% |
| Scalar SHA compression | 113.40 MH/s | 88.61 MH/s | 21.86% |
| Fixed transaction block positions | 114.53 MH/s | 99.45 MH/s | 13.17% |
| Register SHA state | 114.48 MH/s | 115.63 MH/s | -1.01% |
| Register field equality, rejected | 114.82 MH/s | 113.74 MH/s | 0.94% |
| Register SHA state, repeat | 114.51 MH/s | 116.67 MH/s | -1.88% |

Rates are total candidates divided by total measured time across each pipeline's
four trials. All comparisons include the retained Rust CPU implementation except
the explicitly labeled GPU-only diagnostic; that diagnostic uses the same GPU
kernels as the unrolled multiplication/squaring comparison.
Raw matched trials are saved in `rust-engine-results.json`.
Across the two retained-candidate comparisons (eight trials per pipeline), Rust
averaged 116.15 MH/s against 114.49 MH/s for the original kernels, a 1.44% gain.
Both matched sessions improved, by 1.01% and 1.88% respectively. These measurements
establish recovery of the earlier regression on this GPU, not a general claim
that one language is faster or that the optimization ceiling has been reached.
The direct matched comparison against the adapted upstream fixed-key engine
measured 115.71 MH/s for Rust versus 112.34 MH/s for upstream, a 3.00% gain.
Every Rust trial exceeded every upstream trial in that session.
These are complete pipeline measurements, not a
primitive benchmark. Each comparison uses 45 seconds of warmup, eight ABBA/ABBA
trials, one second settling and 12 seconds measured per trial, with 565,248
candidates per batch, 32 candidates per point-walk lane and 16 per C1 thread.
Power/clock/temperature values are snapshots after trials, not interval averages;
some snapshots catch idle gaps. No performance ceiling is claimed.

Nsight Systems CUDA traces located the hashing regression. The original Rust
filter took about 2.61 ms per batch in its short profile; fixed block positions
reduced that to 1.56 ms, and register SHA state reduced it to 0.98 ms. These
128-batch traces are diagnostic, not controlled performance comparisons.
The field-equality experiment removed device `memcmp` calls but failed the
matched performance comparison, so it was reverted. Nsight Compute hardware
counters were unavailable (`ERR_NVGPUCTRPERM`); no driver permissions were changed.

The reproducible trace workload is the ignored `incremental_k_stage_profile`
test built with `--features incremental-k`. Set `PICKAXE_PROFILE_PIPELINE` to
`pickaxe`, `ultrafast` or `rust`, then run that test under `nsys profile
--trace=cuda --sample=none --cpuctxsw=none --stats=true`. It uses synthetic keys
and the same batch geometry, with no transaction broadcasts.

## Remaining adoption gates

The Rust GPU migration is **not complete across hardware backends**. HIP still
uses native kernels and the portable backend still uses WGSL. Those paths need
Rust shader implementations and their own physical-device evidence. This feature
currently establishes a Rust CPU/NVIDIA path. Upstream packaging and maintainer
review remain necessary; retaining both components does not mean the upstream
library's full API or every backend has been ported.

The local NVIDIA pipeline performance gate now passes against both measured
baselines. This does not benchmark every CPU library operation or establish
cross-device performance. These short tests also do not establish long-session
stability or resolve the earlier
unexplained PC restart. The original deployed miner remains the live TUI, and
master/releases have not changed. Preserve the existing engine under
`reference/legacy-engine-by-cyberashven/` when an actual swap is approved; it has
not been moved during this experiment.
