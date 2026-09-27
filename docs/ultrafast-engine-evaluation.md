# UltrafastSecp256k1 integration evaluation

Branch: `experiment/ultrafast-engine`. Baseline: `59c93c17f12244a6a81419fe47df83449f2d530d`.
Upstream release: `v4.6.0`, commit `540ac5b9c910f089c449177ebf000e4c130cab19`.

## Scope and promotion requirements

- Evaluate a Rust-first integration of the maintained upstream engine. Rust owns the integration and PHOTON orchestration; upstream native GPU kernels remain native code. A full rewrite of upstream cryptography in Rust is outside this experiment.
- Keep master and published releases unchanged until correctness, sustained full-pipeline performance, and applicable hardware validation support promotion.
- The user accepts the measured approximately 2% performance loss for upstream adoption, conditional on upstream PR #442 merging. Performance parity is no longer an adoption requirement; the remaining integration and stability gates still apply.
- Exactly one mining process/window. Stop the live miner normally before exclusive GPU tests, and restore it after testing. Synthetic test keys only; no test broadcasts or funded-key experiments.
- Upstream contributions may include binding fixes and independently useful PHOTON optimizations, with reproducible evidence. Do not describe a primitive benchmark as a full mining comparison.
- If a swap is confirmed, preserve the original engine in `reference/legacy-engine-by-cyberashven/`, including source, build instructions, tests, original licensing/attribution, and the exact baseline commit. Until then, keep the original engine active rather than moving it prematurely.

## Current direction: a fully Rust engine

The Rust contribution must match or improve the applicable existing engine's
performance. The earlier approximately 2% native-adoption allowance below does
not apply to a slower Rust rewrite offered upstream as an improvement.

The user now requires the engine implementation itself, including GPU kernels, in Rust. This supersedes adopting the native C++/CUDA candidate described below. Preserve the native evaluation as a reference; its performance measurements do not establish performance for a Rust rewrite. No native optimization PR was opened. Rust-binding PR #442 is ready for review and remains a separate, useful build fix.

The small native follow-up tried the existing alternate point-addition formula and two equivalent inversion tails that remove one field multiplication. Both inversion tails pass the independent 530-value Rust oracle, 13,920-candidate PHOTON checks, and BCH VM validation; the first also passes all 51 upstream CUDA self-tests. Direct full-pipeline before/after results are -0.30% for the first tail and +0.24% for the second, with overlapping trial ranges. Neither establishes a reliable hashrate improvement. Raw results are retained in `docs/ultrafast-full-pipeline-results.json`.

## Earlier decision: conditional upstream adoption; approximately 2% loss accepted

The initial public-API probes do not settle whether an integrated engine can preserve Pickaxe's performance. The user requested the actual integration and complete-pipeline test before concluding. A candidate now compiles upstream CUDA point, field, and scalar arithmetic into Pickaxe's existing GPU-resident PHOTON pipeline; it retains incremental search, batched inversion, dual-signature filtering, and transaction midstates. Rust continues to own orchestration and independent winner checks. This is a native primitive integration, not a benchmark of host buffer transfers.

Both integrated candidates pass the full PHOTON correctness oracle and covenant checks, and are 2.4% and 2.1% slower than their matched Pickaxe baselines. The user accepts this approximate 2% tradeoff for adopting the maintained upstream engine, conditional on upstream PR #442 merging. Proceed with the candidate retaining Pickaxe's fixed-key multiplication once that condition is met; its measured 2.1% loss is acceptable and is not a reason to reject the swap. These measurements apply to the tested NVIDIA GPU and do not establish performance on other hardware.

PR #442 contains only the Rust-binding declaration fix, not the Pickaxe mining integration or its optimizations. Before production adoption, pin and revalidate the merged upstream revision, finish production build/runtime integration, and pass live stability validation in the single TUI. Preserve the current engine under `reference/legacy-engine-by-cyberashven/` when the swap is adopted. Master, release, and live TUI continue using the existing engine while the condition and remaining gates are outstanding; experimental selection is currently confined to ignored tests.

## Findings

- The public Rust/C ABI signer implements BIP-340. PHOTON requires BCH Schnorr. Upstream has a separate BCHN compatibility shim; the generic signer is not a drop-in replacement.
- The GPU generator API takes host scalar buffers and returns compressed public keys to host memory. Pickaxe's current PHOTON pipeline keeps intermediate points/signatures on the GPU and returns bounded winners. This interface difference must be measured and addressed before claiming a performance-neutral swap.
- Stock v4.6.0 Rust bindings fail `cargo check`: unexpected closing delimiter in `bindings/rust/ufsecp-sys/src/lib.rs:376`. A duplicated trailing block redeclares 58 functions already present in the first block. Removing only that duplicated block makes the Rust wrapper build and its tests pass.
- The Windows MSVC CUDA+OpenCL build succeeds after three local compatibility fixes: removing the duplicated Rust declarations, splitting oversized embedded OpenCL string literals without changing their concatenated contents, and using the MSVC byte-swap intrinsic on MSVC while retaining the existing builtin elsewhere. The final patch also backports internal linkage for 46 embedded OpenCL helpers from upstream development commit `73372cec7dc23fee1bac40fae991e3d1ac28bd9d`. These changes are preserved in `tools/ultrafast-v4.6.0-compat.patch`.
- The independently useful Rust declaration fix is submitted as [upstream PR #442](https://github.com/shrec/UltrafastSecp256k1/pull/442), commit `21a7899`. The OpenCL linkage fix was already present in upstream development and is credited as a backport. The remaining Windows fixes are local evaluation patches. No Pickaxe AGPL engine code is included in the upstream contribution.

## Measured results (2026-09-27)

- CPU probe: 64 synthetic vectors pass. Upstream public keys match Rust secp256k1; upstream signatures verify as BIP-340 and fail the BCH verifier as expected. Pickaxe BCH signatures pass its BCH verifier. This confirms a protocol mismatch in the generic signing API, not a defect in BIP-340.
- BCH shim probe: 67 synthetic vectors cross-verify in both implementations; 65 signatures are byte-identical. The two messages at or above the curve order produce different valid signatures because Pickaxe reduces the nonce-generation message modulo the order while upstream follows BCHN's raw-message input. The [BCHN nonce implementation](https://github.com/bitcoin-cash-node/bitcoin-cash-node/blob/master/src/secp256k1/src/secp256k1.c) confirms that difference. Tampered signatures are rejected; zero, order, and all-ones secret keys fail and clear the output. The native shim failure-output-clearing test also passes. These checks are not full PHOTON/winner validation.
- Exclusive GPU probe: all points are checked against Rust secp256k1 for the 16- and 4,096-element batches, including full-width synthetic scalars in the small batch; five boundary/middle points are checked for each larger batch. Each batch runs once to warm up and five more times for median timing, with correctness checks after every call, outside timing. All three available device/backend combinations pass after the backport.
- AMD `gfx1036` initially failed context creation with error 101. A verbose native diagnostic localized the failure to the driver's OpenCL linker: undefined `field_inv_impl`, `scalar_mul_generator_glv_impl`, `point_add_impl`, and `scalar_mul_glv_cl` symbols. Backporting upstream's `static inline` linkage fix resolves initialization and the GPU correctness probe passes. No driver or hardware settings were changed.
- The original miner exited normally before this probe and was restored afterward. A fresh log confirms one miner running at intensity 100, around 116 MH/s, with no reported error. No experimental engine is active in that miner.

Final patched-library results (host transfers included):

| Device / backend | Points per batch | Median time | Million points/s |
| --- | ---: | ---: | ---: |
| NVIDIA RTX 5070 Ti Laptop / CUDA | 262,144 | 25.0149 ms | 10.48 |
| NVIDIA RTX 5070 Ti Laptop / OpenCL | 262,144 | 17.4168 ms | 15.05 |
| AMD gfx1036 / OpenCL | 65,536 | 92.8627 ms | 0.71 |
| AMD gfx1036 / OpenCL | 262,144 | 398.7353 ms | 0.66 |

Final run: all three ignored integration probes pass against the final saved patch (31.95 seconds). Native DLL SHA-256: `06b0baf3fd2d7bec17028ad9cabc003bed99e017f5a61d6ad9c2262ef934a0e6`. Exact output is in `artifacts/ultrafast-evaluation/probes-final.stdout.log`. Earlier repetitions varied (NVIDIA CUDA 10.03–10.62 and NVIDIA OpenCL 13.34–15.43 million points/s at the same largest batch); this is not a sustained thermal comparison.

These rates are **point generation, not mining hashrate**. Pickaxe's observed mining rate is context for the decision, not a controlled complete-pipeline A/B result. No Metal/Apple, discrete AMD, other NVIDIA model, sustained candidate mining, or future CashToken algorithm is validated here.

Additional checks: upstream Rust wrapper 12 smoke tests plus 1 doc test pass; Pickaxe `cargo clippy --locked --all-targets --all-features -- -D warnings` passes; the final `cargo test --locked --all-features --no-fail-fast -- --skip if_cuda --skip if_wgpu` passes with 251 passed, 14 ignored, 18 filtered. Formatting and diff checks pass. The filtered production GPU tests were not run while the miner was active. Native upstream builds emit pre-existing MSVC warnings; the full upstream CTest suite was not run. No claim of a warning-free or fully audited upstream library is made.

## Reproduce on Windows

Use an x64 Visual Studio developer shell with CMake, Ninja, Rust, and CUDA available. These commands create only ignored build artifacts. CUDA architecture `120` matches the measured machine; select the appropriate architecture on other hardware. The patch targets the pinned release, not arbitrary upstream HEAD.

```powershell
$src = 'artifacts/ultrafast-evaluation/upstream'
$build = 'artifacts/ultrafast-evaluation/build'
git clone --depth 1 --branch v4.6.0 https://github.com/shrec/UltrafastSecp256k1.git $src
git -C $src rev-parse HEAD # must equal 540ac5b9c910f089c449177ebf000e4c130cab19
git -C $src apply --check (Resolve-Path tools/ultrafast-v4.6.0-compat.patch)
git -C $src apply (Resolve-Path tools/ultrafast-v4.6.0-compat.patch)
cmake -S $src -B $build -G Ninja -DCMAKE_BUILD_TYPE=Release `
  -DSECP256K1_BUILD_CUDA=ON -DCMAKE_CUDA_ARCHITECTURES=120 `
  -DSECP256K1_BUILD_OPENCL=ON -DSECP256K1_BUILD_CABI=ON -DUFSECP_BUILD_STATIC=OFF `
  -DSECP256K1_BUILD_TESTS=OFF -DBUILD_TESTING=OFF -DSECP256K1_BUILD_BENCH=OFF `
  -DSECP256K1_BUILD_EXAMPLES=OFF -DSECP256K1_BUILD_JAVA=OFF `
  -DSECP256K1_BUILD_BCHN_SHIM=ON -DSECP256K1_BCHN_SHIM_BUILD_TESTS=ON
cmake --build $build --target ufsecp_shared test_bchn_schnorr_fail_clear --parallel 2
& "$build/compat/libsecp256k1_bchn_shim/test_bchn_schnorr_fail_clear.exe"
$env:UFSECP_LIB_DIR = (Resolve-Path "$build/include/ufsecp").Path
$env:PATH = "$env:UFSECP_LIB_DIR;$env:PATH"
cargo test --manifest-path "$src/bindings/rust/ufsecp/Cargo.toml"
$env:PICKAXE_UFSECP_LIBRARY = "$env:UFSECP_LIB_DIR/ufsecp.dll"
cargo test --locked --release --features incremental-k ultrafast_probe::cpu_signature_compatibility -- --ignored --exact --nocapture

# Export the existing static BCH shim for the Rust test; no replacement signer code.
link /NOLOGO /DLL /MACHINE:X64 /OUT:artifacts/ultrafast-evaluation/pickaxe_bchn_probe.dll `
  /DEF:tools/ultrafast-bchn-probe.def `
  "$build/compat/libsecp256k1_bchn_shim/secp256k1_bchn_shim.lib" `
  "$build/src/cpu/fastsecp256k1.lib" bcrypt.lib
$env:PICKAXE_BCHN_LIBRARY = (Resolve-Path artifacts/ultrafast-evaluation/pickaxe_bchn_probe.dll).Path
cargo test --locked --release --features incremental-k ultrafast_probe::bch_shim_signature_compatibility -- --ignored --exact --nocapture

# Stop the live miner normally first; resume it when this offline test finishes.
cargo test --locked --release --features incremental-k ultrafast_probe::gpu_generator_compatibility_and_throughput -- --ignored --exact --nocapture --test-threads=1
```

Stop on a failed command and inspect its diagnostic before continuing. The ignored tests load native code only from the explicitly configured trusted DLL paths. The evaluation does not install a dependency, change a mining backend, or broadcast test transactions.

The test-only Rust module `src/ultrafast_probe.rs` exercises the pinned shared library through its C ABI with existing `libloading`. It adds no runtime backend and cannot activate itself in a production build. Its ignored tests require `PICKAXE_UFSECP_LIBRARY` to name the locally built, trusted library. GPU tests require exclusive use of the GPU.

Local build logs, sources, and measurements are under ignored `artifacts/ultrafast-evaluation/`. No current finding establishes that a complete engine swap preserves hashrate or supports every GPU/token.

## Integrated CUDA candidate

`cuda/ultrafast_compat.cuh` adapts the pinned upstream 4x64 field/point representation to Pickaxe's existing device buffers. It calls upstream mixed-point addition, field multiplication/squaring/inversion, and square-root arithmetic. The first candidate C1 kernel uses upstream scalar addition and multiplication. The second retains Pickaxe's fixed-key Montgomery multiplication while using upstream arithmetic elsewhere. Pickaxe retains the PHOTON transaction format, search scheduling, batched inversion, dual-signature technique, and SHA-256 transaction filtering. Only ignored Rust tests select the candidate kernels; the live CLI cannot activate them.

The baseline and both candidates pass the same correctness harness: 13,920 independently reconstructed candidates each across eight ages and three synthetic keys, partial batches, the 32-bit index boundary, zero/all-pass targets, bounded winner readback, and production-sized batch samples. Each candidate's 216 transaction vectors pass the existing BCH 2026 VM harness in both standard and consensus modes (103 accepted, 113 rejected). Both candidate scalar kernels also pass 19,080 independent BigUint multiplication checks each, including zero, order, limb boundaries, and full-width synthetic values. These checks exercise the actual candidate kernels, not separate replacement implementations.

The timing harness uses one CUDA context and the same 565,248-candidate buffers, 32 candidates per walk lane, 16 candidates per inversion thread, job, key, and target. It warms the baseline for 45 seconds, then alternates baseline/candidate in ABBA/ABBA order, with one second settling and twelve seconds timing per trial. Both pipelines include point generation, BCH signature construction, both transaction hashes, target filtering, and bounded winner readback. It records temperature, power, clocks, and GPU utilization after each trial. Those snapshots may capture idle transitions and are not in-trial averages or evidence of power efficiency. Neither pipeline broadcasts anything. Job setup, key rotation, networking, and transaction submission are outside timing.

Each row below is a separate matched A/B session. Rates aggregate total candidates divided by total measured time over four trials per pipeline; compare each candidate with its own baseline.

| Candidate | Pickaxe baseline (MH/s) | Candidate (MH/s) | Change |
| --- | ---: | ---: | ---: |
| Upstream field, point, and scalar arithmetic | 114.90 | 112.13 | -2.4% |
| Upstream with Pickaxe fixed-key multiplication | 114.16 | 111.79 | -2.1% |

Every candidate trial is below every baseline trial within its matched session. Unlike the earlier point-generation probe, these measure the complete GPU PHOTON search pipeline. Raw trials and correctness counts are committed in `docs/ultrafast-full-pipeline-results.json`.

A short screen covers both candidates with walk lanes 16/32/64 and inversion groups 8/16 (12 configurations). It uses 0.5 seconds warmup and 2 seconds timing per configuration, with no matched baseline, so it is only a tuning screen. The normal candidate's 64/16 and 32/16 results differ by less than 0.1%; the fixed-key candidate's best result is 32/16. It does not establish an improvement worth promoting or an exhaustive optimization limit.

The original TUI was restored after each exclusive GPU session and confirmed mining at intensity 100 without a reported error. There was one live miner. This experiment does not validate prolonged candidate mining, restart stability, an integrated AMD/OpenCL/Metal backend, or future CashToken algorithms. A future upstream version or backend can be retested with this harness; adoption still requires the applicable correctness, performance, and live stability gates.

```powershell
tools/build-ultrafast-candidate.cmd
# With the live miner stopped normally, run serially:
cargo test --locked --release --features incremental-k cuda_photon::incremental_k::incremental_k_correctness -- --ignored --exact --nocapture
cargo test --locked --release --features incremental-k cuda_photon::incremental_k::incremental_k_ultrafast_scalar_oracle -- --ignored --exact --nocapture
cargo test --locked --release --features incremental-k cuda_photon::incremental_k::incremental_k_ultrafast_correctness -- --ignored --exact --nocapture
cargo test --locked --release --features incremental-k cuda_photon::incremental_k::incremental_k_ultrafast_fixed_d_correctness -- --ignored --exact --nocapture
node tools/reward-policy-vm/photon-layout.mjs artifacts/incremental-k/ultrafast-vectors.json
node tools/reward-policy-vm/photon-layout.mjs artifacts/incremental-k/ultrafast-fixed-d-vectors.json
cargo test --locked --release --features incremental-k cuda_photon::incremental_k::incremental_k_ultrafast_comparison -- --ignored --exact --nocapture
cargo test --locked --release --features incremental-k cuda_photon::incremental_k::incremental_k_ultrafast_fixed_d_comparison -- --ignored --exact --nocapture
cargo test --locked --release --features incremental-k cuda_photon::incremental_k::incremental_k_ultrafast_tuning -- --ignored --exact --nocapture
# Resume the original miner afterward.
```

The build script uses the same Windows CUDA/MSVC toolchain as `build-incremental-k.cmd`; its CUDA architecture is specific to the measured host. The VM harness uses its existing pinned Libauth dependency. Results go to `artifacts/ultrafast-evaluation/{full-pipeline-comparison,fixed-d-comparison,tuning}.json`. The integrated candidate needs only the pinned upstream headers; building the public API DLL is necessary only for the separate API probes.
