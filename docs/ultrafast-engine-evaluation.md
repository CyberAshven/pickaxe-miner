# UltrafastSecp256k1 integration evaluation

Branch: `experiment/ultrafast-engine`. Baseline: `59c93c17f12244a6a81419fe47df83449f2d530d`.
Upstream release: `v4.6.0`, commit `540ac5b9c910f089c449177ebf000e4c130cab19`.

## Scope and promotion requirements

- Evaluate a Rust-first integration of the maintained upstream engine. Rust owns the integration and PHOTON orchestration; upstream native GPU kernels remain native code. A full rewrite of upstream cryptography in Rust is outside this experiment.
- Keep master and published releases unchanged until correctness, sustained full-pipeline performance, and applicable hardware validation support promotion.
- Exactly one mining process/window. Stop the live miner normally before exclusive GPU tests, and restore it after testing. Synthetic test keys only; no test broadcasts or funded-key experiments.
- Upstream contributions may include binding fixes and independently useful PHOTON optimizations, with reproducible evidence. Do not describe a primitive benchmark as a full mining comparison.
- If a swap is confirmed, preserve the original engine in `reference/legacy-engine-by-cyberashven/`, including source, build instructions, tests, original licensing/attribution, and the exact baseline commit. Until then, keep the original engine active rather than moving it prematurely.

## Decision

Do not replace the production engine with v4.6.0 through its existing public API. The Rust integration and available GPU primitives work after compatibility fixes, but the library does not supply the resident PHOTON search pipeline or an equivalent public batch-signing interface. Point generation alone measures far below the current miner's observed candidate rate on this machine. This supports rejecting an **as-is** replacement; it does not establish the performance of a future custom PHOTON implementation built from upstream primitives.

Keep the current engine, master, release, and single live TUI unchanged by this experiment. No legacy copy is created because no swap occurred. A future replacement would require a native PHOTON search implementation, then full protocol/winner tests and sustained equivalent-work measurements before promotion. The test module is not a runtime engine selector.

## Findings

- The public Rust/C ABI signer implements BIP-340. PHOTON requires BCH Schnorr. Upstream has a separate BCHN compatibility shim; the generic signer is not a drop-in replacement.
- The GPU generator API takes host scalar buffers and returns compressed public keys to host memory. Pickaxe's current PHOTON pipeline keeps intermediate points/signatures on the GPU and returns bounded winners. This interface difference must be measured and addressed before claiming a performance-neutral swap.
- Stock v4.6.0 Rust bindings fail `cargo check`: unexpected closing delimiter in `bindings/rust/ufsecp-sys/src/lib.rs:376`. A duplicated trailing block redeclares 58 functions already present in the first block. Removing only that duplicated block makes the Rust wrapper build and its tests pass.
- The Windows MSVC CUDA+OpenCL build succeeds after three local compatibility fixes: removing the duplicated Rust declarations, splitting oversized embedded OpenCL string literals without changing their concatenated contents, and using the MSVC byte-swap intrinsic on MSVC while retaining the existing builtin elsewhere. The final patch also backports internal linkage for 46 embedded OpenCL helpers from upstream development commit `73372cec7dc23fee1bac40fae991e3d1ac28bd9d`. These changes are preserved in `tools/ultrafast-v4.6.0-compat.patch`.
- The independently useful Rust declaration fix is submitted as [upstream draft PR #442](https://github.com/shrec/UltrafastSecp256k1/pull/442), commit `21a7899`. The OpenCL linkage fix was already present in upstream development and is credited as a backport. The remaining Windows fixes are local evaluation patches. No Pickaxe AGPL engine code is included in the upstream contribution.

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

Additional checks: upstream Rust wrapper 12 smoke tests plus 1 doc test pass; Pickaxe `cargo clippy --locked --all-targets --all-features -- -D warnings` passes; `cargo test --locked --all-features --no-fail-fast -- --skip if_cuda --skip if_wgpu` passes with 251 passed, 8 ignored, 18 filtered. The filtered production GPU tests were not run while the miner was active. Native upstream builds emit pre-existing MSVC warnings; the full upstream CTest suite was not run. No claim of a warning-free or fully audited upstream library is made.

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
