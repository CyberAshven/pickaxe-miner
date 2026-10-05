# Shared Rust GPU shaders

This folder turns the shared Rust engine (`rust-engine/src`) into the WGSL that
the portable wgpu engine ships. It began as a feasibility probe, hence its
name; the sections after this one are its dated history.

| Path | Purpose |
|---|---|
| `sync_filter.py` | Generates and checks every portable shader. `--write` regenerates `reference/shared-t2/` (17 T2 filters) and `reference/shared-stages/` (the other stages and their DirectX 12/Metal copy) with their `SHA256SUMS`; without `--write` it fails on any drift. CI runs it on every pull request. |
| `builder/` | Compiles the shader crates with rust-gpu (pinned nightly) and translates SPIR-V to WGSL and Metal source |
| `filter/` | Shader crate for the T2 filter, one entry point per layout shift, with its report and measurements |
| `stages/` | Shader crate for signing (A, B, C1), T2 preparation, C2 and C3 |
| `kernel/` | The first hashing probe and its CPU oracle tests |
| `compare_ptx.py` | Compares exported PTX kernels, ignoring comments and source-line annotations |

A normal `cargo build` does not need rust-gpu: `build.rs` checks the committed
WGSL against the recorded hashes of its Rust sources.

## The first probe

The first probe evaluated compiling the production SHA-256 hot path for CUDA
and SPIR-V, then translating SPIR-V to WGSL and Metal source. The probe itself
is not wired into the miner or release workflows. No keys, payout addresses,
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

The generated WGSL passed all 13,056 independent hash cases in browser WebGPU
on NVIDIA (Brave headless) and AMD (Chrome), serially with mining stopped.
WGSL SHA-256:
`d873a6c523df43b097e9dfa2ca15cf4bda370b9050ba060fd1caf59ecff6848e`.
The AMD fixture loader rejected sizes on its first attempt before dispatch;
a fresh module load then passed. Four native arithmetic tests, the complete
host fixture oracle, formatting and warnings-denied Clippy passed.

Remaining: full-pipeline performance,
GPU signatures, complete-target/winner integration, software Vulkan CI and
Apple execution. Generated Metal source has not been compiled by Apple tooling
or run on a Mac. Earlier interface shapes changed native PTX and were rejected.
This probe is not selected by any production miner.

## Shared transaction layout (2026-10-04)

The native T2 block assembler now imports `rust-engine/src/t2_block.rs`.
Its byte layout and padding have one definition, with native byte loads and
portable packed-word/u32-pair primitives at the compiler boundary. The portable
probe imports this exact file, rather than a translated copy of the layout.

All 1,632 independently serialized assembly cases and 13,056 complete hash
cases passed in browser WebGPU on both NVIDIA (Brave) and AMD (Chrome), serially with
the native miner paused. Assembly covers all 17 shifts, carry/borrow cases,
window endpoints, padding and an excess workgroup. The same CPU oracles,
warnings-denied Clippy and formatting passed. Generated WGSL SHA-256:
`f5ffc94c242e85d8fdf9f80b184fa8b08ec79a8ece14799d7485044e3ddb100f`.

All 50 native kernels and the whole PTX remain byte-identical to the baseline
hash above. Keeping the native closure while using a portable byte accessor
preserved the register schedule. Direct bounds comparisons avoided private
range objects in SPIR-V, reducing generated WGSL from 7.4 MB to 1.2 MB.
Earlier interface variants that changed native PTX were rejected.

Run both CPU oracles with `--tests` in place of `--test oracle` above. For GPU
fixtures, `verifySharedHash({ assembly: true })` runs the layout oracle, and
`verifySharedHash()` runs the hash oracle. Each call releases its GPU device.
This is correctness evidence, not a portable mining performance result.

## Shared stages (2026-10-04)

Every other portable mining stage now comes from the shared Rust engine too.
`stages/` holds thin entry points for signing (A: message hash and RFC6979,
B: k·G from the generator table, C1: the BCH Schnorr signature), the T2
window preparation that feeds the shared filter, and the non-T2 hash and
winner stages (C2, C3). They import
`rust-engine/src/{field,point,scalar,sha256,sign,wide,window}.rs`;
`sync_filter.py` compiles them into `reference/shared-stages`. They use the
original stages' bindings and record layouts, so the host swaps pipelines
only. `PICKAXE_WGPU_STAGES=wgsl` restores the original hand-written stages,
and only then is the 34k-line original module parsed.

What portable shaders need from the shared source, without changing native code:

- 64-bit intermediates go through `wide.rs`: native `u64`, SPIR-V two words.
- Limb loops and computed indices use the `at!`, `set!` and `limbs!` macros,
  which expand to the original code natively and to unrolled, unchecked
  access on SPIR-V. A bounds check, a signed remainder or an array iterator
  adds a panic path, and rust-gpu then inlines whole multiplications.
- Array equality and `Debug` are native-only derives; SPIR-V compares words.
- `sign.rs` (word-based signing, binary inverse and Legendre) and
  `window.rs` (transaction words, block schedule, target rule) are
  portable-only; native GPU builds exclude them.
- RFC6979 runs its HMAC chain as a table of steps around one compression,
  so drivers compile one SHA-256 body. Fifteen unrolled copies took AMD's
  compiler 114 s for stage A; the table form takes 0.2 s.

Checks: both native PTX hashes are unchanged (`380967a5…` with
`upstream-rust`, `5ef8fada…` without). Host tests compare every word form,
the forced RFC6979 retry steps, complete signatures, transaction words for
all 17 layouts and the target rule with independent oracles.
`signing_stages_match_cpu_per_window` and the T2 and non-T2 end-to-end tests
pass on NVIDIA and AMD (Vulkan); the browser engine check passes in Chrome on
both (browser WebGPU). `PICKAXE_BUILD_SHARED_STAGES=debug` builds a development shader
with an arithmetic entry point for `shared_stages_arithmetic_matches_integers`;
it is never shipped. Measurements are in the
[GPU code map](../../docs/gpu-sources.md).

## DirectX 12 and Metal loops (2026-10-05)

wgpu translates WGSL with naga. Its HLSL (DirectX 12) and MSL (Metal) writers
emit a loop's `continuing` block at the top of the next iteration and recompute
the loop-body values that block uses there, after the block's own assignments.
SPIR-V (Vulkan) keeps a real continue block, and Chrome's Tint keeps the
values, so both were correct. On DirectX 12 the shared field inverse's inner
loop stored a dummy value for its successor and then tested that dummy instead
of the value it had computed, so it never ended. The compiler then dropped
every result of the stage: the debug arithmetic entry returned zeros on both
GPUs, and on the Radeon stage B returned no points and C1 hung until Windows
reset the driver.

`sync_filter.py` now also writes `pickaxe_shared_stages_dx12_metal.wgsl`. In
that copy each continuing-block value at risk is stored in a function variable
where the loop body computes it, and the continuing block reads the variable. A
value is at risk when it loads a variable that the body writes after it, or
that the continuing block writes. The shared stages have 15 such loop
conditions, all booleans; the T2 filters have none, and generation stops if a
shader without a copy ever needs one. DirectX 12, Metal and browsers other than
Chromium run the copy. Vulkan and Chromium keep the original byte-identical:
interleaved runs found the copy about 0.9% slower in C1 on the RTX 5070 Ti over
Vulkan (no difference on the Radeon). CI regenerates both files.
