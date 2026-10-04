# Shared Rust T2 filter

#### PR #22

Portable builds import the production Rust hashing and transaction-layout
source. CUDA retains its existing compiler and instructions. The original
V3/V4 experiments below were archived at `ff26949`; the subsequent larger-batch
comparison is recorded separately below. Complete GPU signature/field/point
source sharing is still unfinished.

The portable filter imports the production `rust-engine/src/sha256.rs` and
`t2_block.rs`. Seventeen thin entry points adapt workgroups, storage, atomics
and winner output for SPIR-V. Naga generates WGSL and Metal. The existing
portable signature pipeline remains unchanged.

## Build without source drift

`reference/shared-t2` contains generated compiler output, not another maintained
implementation. Every portable Cargo build verifies the SHA-256 manifest of
its source and artifacts. After editing shared Rust, regenerate it with:

```powershell
python tools/shared-gpu-proof/sync_filter.py --write
```

CI runs this command without `--write`: it recompiles with the pinned Rust-GPU
and Naga versions and rejects differences. It also compiles the generated
Metal sources and executes the production filter's independent transaction and
signature checks on software Vulkan. An ordinary portable application build
uses the checked generated files and does not need Rust-GPU installed locally.

## Larger bounded batches

The portable capacity is shared by the browser and native launcher at
33,554,432 candidates. The elapsed-time ladder still starts at 1,024 and
reduces slow dispatches; a signature still covers 65,536 amount pairs.
The native portable allocation schedule now uses this capacity instead of
capping adaptive batches by the tiny startup size. CUDA scheduling is unchanged.

Brave/NVIDIA comparisons used actual browser WASM, 20-second warmups and
45-second trials in B/S, S/B, B/S order. Completed candidates are counted once.
The existing 16M browser averaged 841.409 MH/s; shared Rust at 32M averaged
972.859 MH/s. The three pair differences were +14.95%, +18.47% and +13.56%.
These are local offline measurements, not a 1.6 GH/s result or a universal claim.

At the same 32M batch size, the previous filter measured 950.164, 952.893 and
952.385 MH/s; shared Rust measured 959.650, 953.645 and 949.729 MH/s.
The filters are within measurement noise. Batching supplies the clear gain;
the shared filter alone is not claimed to be faster. Raw counts, elapsed times
and tested WASM hashes are in `batch32-measurements.json`.

Chrome/AMD RDNA 2 used the actual adaptive browser pipeline, with 10-second
warmups and 30-second B/S, S/B, B/S trials. Baseline rates were 30.155,
30.444 and 30.802 MH/s; shared rates were 31.012, 31.278 and 31.364 MH/s.
All three pairs improved (approximately +2.5% on their means). Cold shader
compilation was excluded. Raw trials are in `amd-batch32-measurements.json`.
This is not a comparison against AMD HIP or a claim about untested AMD cards.

The larger dispatch passed on both NVIDIA and AMD: 33,554,319 candidates
matched serial chunks and all 549 returned winners passed independent CPU
reconstruction. Both GPUs also passed the 6,256 boundary/key/job tests. Five
actual browser-generated synthetic rewards passed BCH 2026 standard and
consensus VM checks, and a full compiled recipient-work cycle passed. These
are offline proofs, not additional mainnet wins.

The integrated production WASM is
`4f78967a77b3369f173ad677a48f6c377044aeb17ad02e9274f8ca952eff3a8d`.
It passed the same 6,256 checks on each GPU, a full browser work cycle and five
independent browser reward VM samples after using the common allocation helper.
The host suite passed 297 library and 16 binary tests, with GPU cases excluded
from those counts; GPU tests ran separately and serially. Strict Clippy,
TypeScript, ten browser transport/submission tests and three release-channel
tests passed. A deliberately changed generated shader was rejected by Cargo;
the original was restored and the build passed again.

The local proof harness now warms until a sufficiently large batch completes.
Its previous elapsed-only warmup could end after a cold first shader compilation
and then demand a winner from only a few thousand candidates. That was a test
assumption failure, not evidence of an invalid mining result.

## Correctness

- All 50 exported native CUDA kernels, and the entire PTX file, remained
  byte-identical to dev baseline `64cda6751a378a7e072931b7ade6d0e01bb0ecdd`.
  SHA-256: `380967a534ea4f877556e5db20be7561193361355979287c6f91d4172ea1e7de`.
  This is not a cross-OS application performance claim.
- 6,256 independent GPU signature, transaction and digest checks passed on
  each of NVIDIA RTX 5070 Ti Laptop and AMD Radeon integrated graphics.
- A 4,194,432-candidate NVIDIA dispatch matched serial dispatches and 1,076
  independently reconstructed winners.
- Host oracles passed 1,632 assembly and 13,056 complete-hash cases.
- Browser-generated reward samples passed independent BCH 2026 standard and
  consensus VM checks, including chained unconfirmed claims and tamper rejection.
  A complete actual work-allocation cycle preserved the compiled donation policy.
- The initial candidate exposed a browser shader-uniformity failure before
  dispatch. Proven bounded loads at the workgroup barrier resolved it; subsequent
  browser tests had no uncaptured GPU or page errors.
- The final host adapter uses one control upload for both preparation and filtering.
  Both GPUs' 6,256-case checks, five browser reward VM samples and strict Clippy
  passed again after that change.

## V3 measurements

One GPU workload at a time; native mining paused; offline synthetic jobs.
No power, clock or driver settings were changed. Work is completed candidates,
counted once. Browser measurements use normal key and recipient rotation.

Brave on NVIDIA: 45-second trials with 20-second warmups, B/S, S/B, B/S order:

| Pair | Current browser MH/s | Shared filter MH/s | Difference |
| --- | ---: | ---: | ---: |
| 1 | 842.269 | 850.269 | +0.95% |
| 2 | 844.333 | 832.779 | -1.37% |
| 3 | 847.696 | 835.826 | -1.40% |

Means: 844.766 versus 839.625 MH/s (-0.61%). This does not establish a
non-regression, so the current browser filter is retained. Unpaired earlier
trials near 928 MH/s were not used to claim an improvement.

AMD native portable pipeline: alternating 30-second trials with 10-second warmups:

| Pair | Current WGSL MH/s | Shared filter MH/s |
| --- | ---: | ---: |
| 1 | 27.8846 | 28.9465 |
| 2 | 27.8930 | 29.3568 |
| 3 | 28.3528 | 28.9619 |

Means: 28.0435 versus 29.0884 MH/s (+3.73%). This compared the filter within
the same complete portable pipeline; both variants used the candidate host
adapter, including its extra control upload. It is not a HIP comparison or an
Apple performance result. No default backend changed.

## Reproduce the archived experiment

Compile the generated files before enabling the candidate feature:

```powershell
$env:PICKAXE_BUILD_SHARED_FILTER = '1'
cargo +nightly-2026-04-11 run --locked --manifest-path tools/shared-gpu-proof/Cargo.toml --release -p pickaxe-shared-hash-builder
Remove-Item Env:PICKAXE_BUILD_SHARED_FILTER
cargo test --locked --release --lib --no-default-features --features shared-rust-t2 --no-run
```

The archived builder checks that all 17 entry points exist and validates generated
SPIR-V through Naga. It writes ignored files under `artifacts/shared-gpu-proof/filter`.
That prototype required explicit generation. The integrated build above adds
the generation and drift checks; use its command for the current source.

With mining stopped, run the ignored `wgpu_photon::tests::t2_gpu_end_to_end_boundaries_and_rotations`
and `t2_gpu_large_dispatch_matches_serial_and_cpu` tests serially. Adapter ordinals
are local to each machine. `t2_gpu_interleaved_pipeline_benchmark` compares
the current 64-lane WGSL filter with the shared candidate when this feature is on.
Use the same browser and device for browser comparisons; do not compare a
browser result with native Vulkan as a matched pair.

No Apple hardware was available. Successful source translation or Mac compilation
must not be described as live Apple GPU mining or a performance result.

## Archived one-upload adapter (V4)

Same 45-second browser trials, 20-second warmups and balanced B/S, S/B, B/S order:

| Pair | Current browser MH/s | Shared filter MH/s | Difference |
| --- | ---: | ---: | ---: |
| 1 | 852.370 | 834.010 | -2.15% |
| 2 | 854.550 | 825.206 | -3.43% |
| 3 | 814.830 | 847.202 | 3.97% |

Means: 840.583 versus 835.473 MH/s (-0.61%). Thermal/run variation remains
visible; these results do not prove a universal slowdown, but they do not meet
the required no-regression gate either. The shared filter is archived, not
selected by PR #22. The default production browser and native CUDA are retained.

Tested browser WASM hashes:

- Verified current browser: `434a9966e8c096e42c7f6992c136bbb75a1bf45e8f69c27144cd9d3b24d0ca78`
- V3: `69970f340185b61d2806833b5bf81cc548179c813c3f5a16cae0ac6c9aa7c62d`
- V4: `f3068b712b18ad68d091209c9d653143e67cef5b7dc831b1947134b52555900e`

The current browser's mainnet acceptance/propagation proof is documented in
`docs/browser-t2.md`. V3/V4 reward samples were offline synthetic VM tests;
they are not claimed as additional mainnet wins. Raw comparison data and
private reward evidence are retained locally outside version control.
