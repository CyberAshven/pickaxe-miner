# Shared Rust T2 filter experiment

#### PR #22

This is an archived, opt-in compiler/performance experiment based on PR head
`57bf702fea7d64c61599a79bb8f4e3e454c441e7`. It is not selected by the
production browser or native miner. Complete GPU signature/math sharing is
not implemented by this experiment.

The portable filter imports the production `rust-engine/src/sha256.rs` and
`t2_block.rs`. Seventeen thin entry points adapt workgroups, storage, atomics
and winner output for SPIR-V. Naga generates WGSL and Metal. The existing
portable signature pipeline remains unchanged.

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

## Reproduce the opt-in experiment

Compile the generated files before enabling the candidate feature:

```powershell
$env:PICKAXE_BUILD_SHARED_FILTER = '1'
cargo +nightly-2026-04-11 run --locked --manifest-path tools/shared-gpu-proof/Cargo.toml --release -p pickaxe-shared-hash-builder
Remove-Item Env:PICKAXE_BUILD_SHARED_FILTER
cargo test --locked --release --lib --no-default-features --features shared-rust-t2 --no-run
```

The builder checks that all 17 entry points exist and validates generated
SPIR-V through Naga. It writes ignored files under `artifacts/shared-gpu-proof/filter`.
This archived prototype intentionally requires that explicit generation step;
automatic CI generation and drift checks are required before production integration.

With mining stopped, run the ignored `wgpu_photon::tests::t2_gpu_end_to_end_boundaries_and_rotations`
and `t2_gpu_large_dispatch_matches_serial_and_cpu` tests serially. Adapter ordinals
are local to each machine. `t2_gpu_interleaved_pipeline_benchmark` compares
the current 64-lane WGSL filter with the shared candidate when this feature is on.
Use the same browser and device for browser comparisons; do not compare a
browser result with native Vulkan as a matched pair.

No Apple hardware was available. Successful source translation or Mac compilation
must not be described as live Apple GPU mining or a performance result.

## Final one-upload adapter (V4)

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
