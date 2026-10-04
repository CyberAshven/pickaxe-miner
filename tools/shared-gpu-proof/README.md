# Shared Rust GPU feasibility probe

#### PR #22

This isolated, offline probe evaluates compiling the production SHA-256 hot
path for CUDA and SPIR-V, then translating SPIR-V to WGSL and Metal source.
It is not wired into the miner or release workflows. No keys, payout addresses,
RPC connections or mining submissions are used by the probe.

The shader imports `rust-engine/src/sha256.rs` directly. It runs the T2
compression chain (resumed first block, precomputed middle schedule, final
block, second hash and round-60 rejection). Transaction assembly, signatures,
target comparison beyond the early rejection, winner collection and pipelined
batch scheduling are not migrated by this first probe.

## Reproduce

Keep the GPU free before running GPU validation. Compilation and the following
CPU oracle do not create a GPU device:

```powershell
$env:CARGO_TARGET_DIR = "$PWD/artifacts/shared-gpu-proof/host-target"
$env:PICKAXE_PROOF_FIXTURES = "$PWD/artifacts/shared-gpu-proof/fixtures"
cargo +stable test --locked --manifest-path tools/shared-gpu-proof/Cargo.toml --release -p pickaxe-shared-hash-proof --test oracle -- --nocapture

$env:CARGO_TARGET_DIR = "$PWD/artifacts/shared-gpu-proof/target"
cargo +nightly-2026-04-11 run --locked --manifest-path tools/shared-gpu-proof/Cargo.toml --release -p pickaxe-shared-hash-builder
```

Rust-GPU is pinned to the `v0.10.0-alpha.1` commit
`e2e4d529a5ead1364228530d301ebbca4ef90262`, with its supported nightly.
The lockfile pins glam 0.31.0, matching that revision's own lockfile. Newer glam
releases changed which vector types are available without default features.
The CUDA build continues using the existing `nightly-2026-04-02` toolchain.
On Windows the Rust-GPU compiler build also needs the Visual Studio C++ tools.

The oracle compares 13,056 complete double hashes with RustCrypto, covering all
17 layout shifts, amount carries/borrows, both sign rules, early-rejection
limits and excess invocation indices. Generated fixture files are synthetic.
This validates hashing, not signatures, covenant execution or a live payout.

## Acceptance

1. Validate generated SPIR-V, WGSL and Metal source; a source translation is not
   an Apple GPU execution test.
2. Execute the same oracle fixtures on GPU before timing anything.
3. Compare native PTX with the baseline built using the same CUDA toolchain.
   A full-miner performance comparison is still required even if PTX matches.
4. Extend to shared T2 assembly and scheduling only after this first proof.
5. Preserve each GPU's verified native performance. Measure OS/driver differences
   on the same hardware and report the browser gap separately.

No existing backend is deleted or replaced by this probe. Software Vulkan CI
can exercise correctness, but hardware performance gates require real NVIDIA,
AMD and Apple GPUs. Future multi-platform release artifacts must come from the
same commit and publish only on explicit release instructions.

## Upstream research

- [Rust running on every GPU](https://rust-gpu.github.io/blog/2025/07/25/rust-on-every-gpu/)
  demonstrates shared Rust source; it is not a Pickaxe speed benchmark.
- [Shrec's Metal suggestion](https://github.com/CyberAshven/pickaxe-miner/pull/22#issuecomment-5971673647)
  points to the MIT-licensed UltrafastSecp256k1 Metal arithmetic. At inspected
  commit `80b08d5ad77b88cb6eefff7c63eb7780309b2d8c`, its Schnorr signing path is
  BIP-340. Pickaxe's BCH Schnorr behavior must be preserved; that signer is not
  a direct replacement. Field/point arithmetic remains a candidate for study.

## Verified boundary revision (2026-10-04)

The native-preserving interface adapter passed the first compilation gate:
all 50 exported native kernels and the entire generated PTX file are byte-for-byte
identical to the unchanged dev baseline. Both SHA-256 hashes are
`380967a534ea4f877556e5db20be7561193361355979287c6f91d4172ea1e7de`.
The baseline is dev commit `64cda6751a378a7e072931b7ade6d0e01bb0ecdd`,
built for sm_120 with the pinned native compiler and upstream-rust feature.
This does not prove identical whole-application speed across operating systems.

The generated WGSL passed all 13,056 independent hash cases on NVIDIA WebGPU
(Brave headless) and AMD WebGPU (Chrome), serially with mining stopped.
WGSL SHA-256:
`d873a6c523df43b097e9dfa2ca15cf4bda370b9050ba060fd1caf59ecff6848e`.
The AMD fixture loader rejected sizes on its first attempt before dispatch;
a fresh module load then passed. Four native arithmetic tests, the complete
host fixture oracle, formatting and warnings-denied Clippy passed.

Remaining: shared T2 transaction-block assembly, full-pipeline performance,
GPU signatures, complete-target/winner integration, software Vulkan CI and
Apple execution. Generated Metal source has not been compiled by Apple tooling
or run on a Mac. Earlier interface shapes changed native PTX and were rejected.
This probe is not selected by any production miner.
