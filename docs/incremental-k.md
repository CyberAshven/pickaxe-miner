# Incremental-k experiment — 2026-09-26

Branch `perf/incremental-k`, based on remote master/release commit `bb92508908b7a6e8235440168c6fe08140964890`.

**Result: 1.95x median complete-pipeline throughput**: 27.128 to 52.928 million candidates/second in the original controlled comparison. After explicit user approval, the candidate was integrated behind the opt-in `incremental-k` feature and deployed to the single live TUI in `D:\pickaxe-candidate`. It found a natural winner and logged its parent/reward submission as accepted. The user subsequently authorized promoting `fd95cbc` to master and continuing measured optimization rounds. The default release build and `D:\pickaxe-live` remain unchanged. This is the best verified candidate from these trials, not proof of an absolute optimization ceiling.

## Round 7: researched and rejected binary/shared inversion prototypes

The user's request for broader internet research led to [gECC](https://arxiv.org/html/2501.03245v1). Its Gather-Apply-Scatter design combines thread products, performs fewer inversions, and distributes the inverses. The authors' [MIT implementation](https://github.com/CGCL-codes/gECC/blob/322c1c143d6a886747308f5b4d88603d6e30a48a/include/gecc/arith/batch_ec.h) was inspected at commit `322c1c1`. Its SM2/A100 results do not establish a PHOTON/secp256k1/Blackwell gain. No third-party implementation was imported into product code.

Two offline C1 prototypes reused Pickaxe's field arithmetic: a per-thread binary inverse, then a 64-thread shared product tree with one binary inverse per block. At eight candidates per thread the latter shares an inverse across 512 candidates. Empty lanes contributed one and reached every barrier; modular halving preserved the 257th carry bit. The binary coefficient invariant was `u=a*x, v=a*y (mod p)`. These variable-time experiments used only synthetic keys and were never deployed.

Both passed all 13,920 independent CPU reconstruction checks, eight age layouts, partial batches, scalar boundaries, target/readback limits and full-batch samples. Compiler reports showed 88 registers/576-byte stack for binary inversion, and 94 registers/576-byte stack/4096-byte shared memory for the tree, with no spills in the measured batched entry.

The unchanged comparison harness measured complete pipelines, using 45 seconds of warm-up and eight ABBA ABBA eight-second windows with one-second inter-window warm-ups:

| Candidate | Master median, million/s | Candidate median, million/s | Difference |
| --- | ---: | ---: | ---: |
| Per-thread binary inverse | 104.731 | 92.229 | -11.94% |
| Shared 64-thread binary inverse | 107.218 | 106.831 | -0.36% |

Neither established an improvement. The shared result is within variability; telemetry reached 87 C and is retained in the raw results. All task-owned source changes were removed and test PTX restored. Master remains `6710112`; the unchanged best miner was restarted after the sole offline worker exited. No CI or additional live candidate validation was needed for rejected, removed prototypes.

Ignored `artifacts/incremental-k/round7/` retains both patches, kernels, compiler logs, correctness logs, raw comparisons and a primary-source research note. Other research leads include per-limb point-buffer coalescing from gECC and batch/occupancy tuning from [BitcoinAddressFinder's performance notes](https://github.com/bernardladenthin/BitcoinAddressFinder/blob/main/docs/performance.md). These remain hypotheses; this result does not rule out other cooperative inversion designs.

## Round 6: rejected centered affine batches and register-limit trials

The next approach generated affine points around a lane's center scalar. Positive and negative offsets share the same x-coordinate denominator; a prefix product supplies their inverses together. A dedicated C1 entry reused the production signing helper while skipping normalization for these Z=1 points. The derivation used the [EFD affine group formulas](https://www.hyperelliptic.org/EFD/g1p/auto-shortw.html); [VanitySearch's GPU point batching](https://github.com/JeanLucPons/VanitySearch/blob/master/GPU/GPUCompute.h) was inspected as a conceptual comparison, without importing source. This still enumerated the original consecutive public search scalars and retained the funded-key separation.

Both 32- and 16-candidate lane shapes passed the 13,920 independent cases. Full-batch signature sampling was expanded to include the first, second and last lane at every offset within each group. The 128-register builds required 1024/832 bytes of stack and 128/112 bytes of spill stores respectively. Raising the compiler limit to 256 removed spills: actual allocation became 184/182 registers, with 992/736 bytes of stack. The affine C1 used 79 registers and no stack or spills. All changed compiler variants also passed the independent checks before timing.

Each comparison used the same full pipeline and master reference kernels, 45 seconds of warm-up, eight alternating eight-second windows and one-second inter-window warm-ups. The reference retained 32 candidates per lane. The candidate changed both point generation and C1 together; host geometry changed with the selected kernel pair. A final trial kept the original algorithm and C1 and changed only the point-generator register limit (190 actual registers, 264-byte stack, zero spills):

| Candidate | Order | Master median, million/s | Candidate median, million/s | Difference |
| --- | --- | ---: | ---: | ---: |
| Affine 32, register limit 128 | ABBA ABBA | 103.139 | 90.483 | -12.27% |
| Affine 16, register limit 128 | BAAB BAAB | 104.590 | 100.570 | -3.84% |
| Affine 16, register limit 256 | ABBA ABBA | 107.353 | 99.805 | -7.03% |
| Original walk, register limit 256 | BAAB BAAB | 109.363 | 109.251 | -0.10% |

None established a speed record. Telemetry varied and included isolated idle/low-utilization samples, retained in the raw JSON rather than discarded. Removing reported spills did not establish a speed gain; the precise limiting hardware mechanism remains unproven. An Nsight Compute attempt with clock control disabled was denied access to GPU counters (`ERR_NVGPUCTRPERM`). Its partial, instrumented timings are excluded from these comparisons, and the diagnostic child was stopped. No driver permission or clock setting was changed.

All task-owned CUDA/Rust candidate changes were removed and saved as an ignored patch. No candidate reached the live TUI or master. The unchanged verified miner was restarted after confirming GPU tests had exited. `artifacts/incremental-k/round6/` contains the candidate patch, compiler logs, correctness evidence, reference PTX, all four raw comparisons and the failed profiler log. The timing evidence rejects these implementations; it does not prove an absolute optimization ceiling or rule out other affine designs.

## Round 5: rejected shared C3 SHA-256 schedules

A new Nsight Systems capture of the master pipeline's existing nine-shape tuning sweep attributed 49.0% of CUDA kernel time to C1 signing, 32.4% to incremental point generation and 18.6% to C3 filtering. Median kernel durations across that sweep were 1.264, 0.833 and 0.485 ms respectively. These aggregated profiling measurements locate work; they are not a controlled speed comparison or live rate.

The next prototype expanded the constant transaction blocks 8 and 9 once per CUDA block, stored their SHA-256 schedules in 512 bytes of shared memory, and reused them across candidates and both signature variants. A compile-time assertion kept signature bytes outside those shared blocks; the synchronization occurred before the partial-batch return. This explored NVIDIA's [shared-memory reuse guidance](https://docs.nvidia.com/cuda/cuda-programming-guide/02-basics/writing-cuda-kernels.html), without importing outside code or changing transaction bytes or the kernel ABI.

All 13,920 independent correctness cases and full-batch samples passed, including all age layouts. The prototype C3 needed 128 registers, a 392-byte stack and 228 bytes each of spill stores and loads. An eight-window ABBA ABBA full-pipeline comparison after 45 seconds of warm-up measured **108.726 million/s master versus 106.654 million/s candidate (-1.91%)**. Samples ranged from 79–84 C and 2685–2707 MHz. Both C1 comparison slots used identical Montgomery PTX hashes; only the C3 filter set changed.

The prototype was rejected and never deployed live. The temporary comparison-harness changes were removed, and the single verified miner was restarted with its unchanged C1/C3 kernels. No performance change was promoted. Ignored `artifacts/incremental-k/round5/` retains the prototype, compiler report, independent correctness log, interleaved measurements, harness patch and fresh Nsight report. The profile supports investigating inversion/normalization in C1 and point generation next, rather than repeating these SHA schedule experiments.

## Round 4: rejected fixed-eight C1 specialization

Starting from master `6710112`, a CUDA-only offline prototype fixed C1 to eight candidates per thread and unrolled both prefix-product and reverse-signing loops. NVIDIA's [private-array indexing guidance](https://developer.nvidia.com/blog/fast-dynamic-indexing-private-arrays-cuda/) motivated replacing dynamic local-array access with constant indexes. This was a restricted test kernel, not a replacement for the generic production entry point: other batch sizes were intentionally unsupported and it was never deployed live.

The 128-register build reduced the C1 stack from 672 to 160 bytes without spills, but used 128 rather than 75 registers. A second build capped registers at 96 and required a 232-byte stack, 72 bytes of spill stores and 104 bytes of spill loads. Both passed 13,920 independent signature/transaction cases and full-batch lane samples using the eight-candidate configuration.

Each full-pipeline comparison used the same workload as round 3, 45 seconds of warm-up and eight interleaved eight-second windows:

| Prototype | Order | Master median, million/s | Candidate median, million/s | Difference |
| --- | --- | ---: | ---: | ---: |
| Fixed eight, 128 registers | ABBA ABBA | 94.964 | 95.708 | +0.78% |
| Fixed eight, 96 registers | BAAB BAAB | 101.174 | 102.544 | +1.35% |

The first run sampled 86–87 C and 2385–2452 MHz; the second sampled 77–83 C and 2250–2707 MHz. These differences are within the observed variability and do not establish a repeatable speed record. Both prototypes were rejected. No product source or live kernel changed. The sole live miner was stopped cleanly for serial GPU testing, then the verified Montgomery runtime was restarted. Ignored `artifacts/incremental-k/round4/` retains prototype source, compiler resource reports, correctness logs, reference PTX and raw comparisons.

## Round 3: Montgomery scalar multiplication

CUDA C1 replaces 32 challenge-indexed table additions with an eight-limb Montgomery product. For R=2^256, the existing table entry at byte 31, digit 128 is d*R/2 mod n; doubling it supplies d*R mod n, so Montgomery(e, d*R) returns e*d mod n. This preserves full-width random search keys, all signatures, the kernel ABI and persistent allocations. HIP keeps the existing table algorithm. The algorithm was researched against NVIDIA's [CGBN Montgomery documentation](https://github.com/NVlabs/CGBN/blob/master/docs/CGBN.md); no external library or source was imported.

Three interleaved comparisons used 45 seconds of initial GPU warm-up, one second before each window and eight seconds measured per window. Eight windows per run used ABBA ABBA and BAAB BAAB. Same full-width key, target, 262,144-candidate batches, complete incremental pipeline and compact winner readback:

| Order | Previous C1 median, million/s | Montgomery C1 median, million/s | Gain |
| --- | ---: | ---: | ---: |
| ABBA ABBA | 97.049 | 105.424 | 8.63% |
| BAAB BAAB | 96.990 | 107.512 | 10.85% |
| ABBA ABBA, final production source | 96.743 | 106.991 | 10.59% |

Temperatures/clocks varied: 84–86 C and 2362–2692 MHz in the first run, 79–81 C and 2400–2707 MHz in the second. One second-run baseline telemetry sample had 85% utilization; the other second-run samples had 97–99%. The repeated advantage is stronger evidence than the highest 110.269 million/s window; it is not a guarantee of that speed under thermal throttling. Before this round, the unchanged live executable and C1 hashes were checked when speed fell to 81.6 million/s: NVIDIA reported active software thermal slowdown at 87 C. No overclock, power, fan or system thermal setting was changed by this work.

The final comparison ran at 76–79 C. Reference clocks were 2625–2677 MHz and candidate clocks 2700–2707 MHz, so these are complete-pipeline laptop results including hardware clock behavior, not equal-clock instruction throughput.

The final production source passed 19,080 direct GPU scalar products against Rust BigUint, including zero, order-minus-one/two, limb boundaries and deterministic random full-width operands. It also passed 13,920 independently reconstructed signatures/transactions and full-batch lane samples. The feature-enabled release suite passed 258 tests, zero failures, five ignored. Explicit VM validation matched all 216 expectations (103 accepted, 113 rejected) under both BCH 2026 standard and consensus rules. Formatting, Rust 1.98 all-target/all-feature Clippy with warnings denied, and all four [CI jobs for 935a1e1](https://github.com/CyberAshven/pickaxe-miner/actions/runs/36261741047) passed, including Windows, Linux, HIP gfx1036 compilation and BCH VM proof. Evidence is in ignored `artifacts/incremental-k/round3/`.

The single live TUI loaded C1 SHA-256 `520cb82795f0c8bfc8ad2ddef10fe1f73ec09c08391ce7ad0af872aa0cd97139`. After resuming, its candidate counter advanced by 22,144,704,318 over 220 seconds (Unix 1790446674–1790446894): **100.658 million/s including settlement/reconnect time**. This excludes the earlier paused interval; the TUI session average includes that pause and is not the comparison metric. It recorded three search-key rotations, one verified winner, zero rejected/stale winners and zero pending winners at the endpoint. One settlement-authority retry recovered automatically, with application-recorded submission acceptance at Unix 1790446797:

- Parent: `8000000003d1226f990c9bc1279b15f6e0199118e84751b747a7f78c61e6ab1f`
- Reward: `8d1e1cf6f56f5570d2f498f811f386ec7ed92fe628164cd3f3a9f05ef99f23c0`

The TUI subsequently observed that reward as the live baton. This is a short live validation, not independent block-confirmation proof or a long soak. Initial sampled live rates were about 105–109 million/s and later fell to about 94 million/s as a GPU sample reached 87 C. The repeated controlled gain is the promotion criterion; neither a peak nor a pause-diluted session average establishes a regression. Exactly one miner and one miner terminal remained running.

The direct arithmetic test is `incremental_k_montgomery_scalar_oracle` (ignored, explicit CUDA run). `tools/build-incremental-k.cmd` now builds six runtime PTX files plus `scalar-check.ptx`, a test-only export of the production multiplication. For a controlled comparison, retain the previous C1 as `photon_c1_reference.ptx` beside the test executable, install current kernels, and run `incremental_k_c1_comparison -- --ignored --nocapture --test-threads=1`. Set `PICKAXE_C1_REVERSE=1` for BAAB BAAB. The earlier raw reverse-run log used swapped PTX aliases (variant 0 was Montgomery); the committed comparison test always labels variant 0 as reference and variant 1 as current.

## Round 2: rejected C1 memory and arithmetic candidates

Research reviewed NVIDIA's [shared-memory spilling guidance](https://developer.nvidia.com/blog/?p=105101), [local-memory behavior](https://docs.nvidia.com/cuda/cuda-programming-guide/02-basics/writing-cuda-kernels.html), and [extended-precision carry instructions](https://docs.nvidia.com/cuda/parallel-thread-execution/contents.html). Other implementations, including [UltrafastSecp256k1](https://github.com/shrec/UltrafastSecp256k1), use Montgomery batch inversion; Pickaxe already uses that algorithm. External throughput claims were not treated as comparable PHOTON results.

- Shared-memory spilling alone: both versions compiled to 70 registers, a 704-byte stack and no spill loads/stores. The large local array is not a register spill, so this pragma was not pursued.
- Explicit shared prefix array: stack fell to 192 bytes, but each block needed 32 KiB shared memory and 94 registers. All 13,920 independent cases and full-batch samples passed. ABBA ABBA full-pipeline medians were **96.176 baseline vs 88.350 candidate million/s**: rejected.
- Scalar addition/subtraction via explicit PTX carry chains: 66 registers, 672-byte stack; the same independent correctness checks passed. ABBA ABBA medians were **100.353 baseline vs 96.776 candidate million/s**: rejected.

Both comparisons used the same full-width key, 262,144-candidate batches, one-second warmups and four-second measured windows. The miner was stopped cleanly through its TUI input before serial GPU tests and the unchanged best live executable was restored afterwards. A GPU sample between tests was 66 C; these later rates must not be compared directly against the hotter initial benchmark as a code improvement. Rejected source variants, build resource reports, correctness logs, raw timings and the temporary comparison-test patch are retained in ignored `artifacts/incremental-k/round2/`, not product code.

CI also found two issues missed by the original local gate: current Rust requires `as_chunks` for these fixed-size slices, and the new hardware integration test lacked the existing `if_cuda` naming convention used by non-GPU CI. The fix keeps scalar-boundary and funded-identity guards in a separate CPU test, while preserving the hardware test for explicit CUDA runs. Neither fix changes mining arithmetic or claims a performance gain.

## Live integration and follow-up measurements

The CUDA worker now returns the explicit signing scalar separately from the commitment nonce. CPU verification independently reconstructs the signature and checks the full transaction digest and target. Batches stop at the scalar-index boundary; the existing sweep counter rotates the unfunded search key after 2^32 candidates. Job replacement and key rotation reload the uniform message and signing state. A guard rejects a search identity equal to the reward destination identity; reward signing continues to use RFC6979. Existing freshness, settlement, journal and donation checks remain in place.

At 20:11 Arabia Standard Time on September 26, the only running miner was `D:\pickaxe-candidate\pickaxe_miner.exe`, in the terminal titled `Pickaxe Miner - incremental-k`. Its TUI log recorded about 57–60 million candidates/s average during the initial minutes, a 73.711 million instantaneous peak, successful key rotation, one verified winner and zero rejected winners. These live numbers are not an A/B comparison. One settlement-authority retry reconnected automatically, followed by `submission accepted` at Unix time 1790442734:

- Parent: `800000000175105a8059af2fe2ccc7b2835fe6e510ffe887468c89ceaa28e93c`
- Reward: `d09e48be6d76e6cbdf06c77cf4e5550be40346154d6d1b531fb3cf987ebe33ff`

This is application-recorded mainnet submission acceptance, not an independently checked block confirmation or long soak. Process, window title and TUI-generated logs were checked; screen capture returned black, so visual rendering was not verified. A later live GPU sample was 78 C, 129.35 W, 100% utilization.

Further trials kept 32 candidates per walking lane and eight per C1 thread:

- Nsight Systems put median C1 signing at about 1.669 ms, point walking at 0.887 ms and C3 at 0.545 ms in the sampled run. C1 is the next measured bottleneck.
- A 16-word unrolled SHA schedule passed correctness but repeated comparison gave about 58.074 versus 57.898 million candidates/s: less than 1%, within run variability. Removed.
- Lane/inversion batching sweeps did not establish a repeatable improvement. Increasing C1 inversion capacity to 64 grew PTX from about 5.5 MB to 19.8 MB; 64-candidate batches fell to roughly 35–44 million/s, and 32-candidate batches to 47–57 million/s versus roughly 61 million/s for the retained shape in that trial. Removed.

The tested tuning approach follows NVIDIA's [CUDA best practices](https://docs.nvidia.com/cuda/archive/12.8.0/cuda-c-best-practices-guide/) and [private-array indexing discussion](https://developer.nvidia.com/blog/fast-dynamic-indexing-private-arrays-cuda/). Rejected experiment patches, profiler output and measurements remain in ignored local evidence; their implementations are not shipped. Larger changes to C1 would require another correctness and controlled performance cycle. GPU tests/benchmarks must run serially while the live miner is stopped, to preserve the user's single-miner constraint.

## What was measured

RTX 5070 Ti Laptop GPU, NVIDIA driver 616.92, nvcc 13.4, sm_120, Rust 1.94 release optimization. Both paths use the same full-width synthetic signing key (32 bytes of 0x11), target, 615-byte transaction shape, 262,144-candidate batches, eight-slot winner buffer, C1 signing, C3 dual hashing/residue selection, and synchronous compact winner readback. Setup and table generation/upload are excluded; kernel dispatch, seed work, computation, synchronization and readback are included. One counted candidate means one mathematically valid BCH signature/transaction; the two internal sign-variant hash attempts are NOT counted twice.

Eight windows in ABBA ABBA order; each has one second of warmup then at least four seconds measured by host monotonic wall clock. Baseline changes the commitment nonce and runs RFC6979 plus full fixed-base kG. Candidate fixes commitment nonce 0x12345678, seeds each GPU lane at k=base+lane+1 and advances by stride*G for up to 32 candidates per lane. This interleaved walk covers consecutive k without overlapping lanes. The shared downstream pipeline is unchanged except the experimental C3 build retains the template nonce and returns a scalar index.

| Round | Pipeline | Candidates | Seconds | Million candidates/s |
| --- | --- | ---: | ---: | ---: |
| 0 | baseline | 119537664 | 4.004399 | 29.852 |
| 1 | incremental | 228327424 | 4.001703 | 57.058 |
| 2 | incremental | 216268800 | 4.002520 | 54.033 |
| 3 | baseline | 104595456 | 4.012560 | 26.067 |
| 4 | baseline | 109576192 | 4.013700 | 27.301 |
| 5 | incremental | 202637312 | 4.000931 | 50.648 |
| 6 | incremental | 207618048 | 4.006323 | 51.823 |
| 7 | baseline | 108265472 | 4.016433 | 26.956 |

The laptop reached 86 C during measurement; one full-width-run sample was 88.82 W, 2370 MHz, 100% GPU utilization. No Pickaxe miner process was found before or after benchmarking; the existing live log last changed at 19:23:27 Arabia Standard Time. We did not stop/restart it. Brave appeared in NVIDIA's process inventory; background desktop load was not controlled. These are thermally variable local measurements, not a sustained production guarantee. The previously reported 54 MH/s peak was not used as the comparison denominator and cannot be multiplied by this ratio to promise a new peak.

All six kernels were rebuilt from source and SHA-256 matched the files used in the measurements. Generated kernels and test executable live outside the deployed miner. Raw measurements, telemetry snapshots, hashes, correctness logs and vectors are in `artifacts/incremental-k/` (ignored local evidence).

## Correctness evidence

- 13,920 candidates independently reconstructed using CPU libsecp256k1 point multiplication and BigUint scalar arithmetic, then verified with the BCH Schnorr verifier and full transaction HASH256. Exact GPU winner sets/digests matched, including misses.
- Eight ages: 0, 16, 17, 127, 128, 32767, 32768, 65534; three synthetic keys (1, 7, and full-width 0x11); counts 1, 65 and 257; bases 0, 1, 65520 and 2^32-257. Final scalar 2^32 succeeds without wrapping to zero. Out-of-range and oversized batches are rejected; zero count is empty.
- Zero/all-pass targets and four-slot winner truncation passed. A full 262,144-candidate all-pass launch counted every candidate; independent signature samples checked lane boundaries and the final 32-step tail.
- 216 reconstructed transaction samples passed BCH 2026 consensus AND standard VM expectation checks: 103 valid winners accepted, 113 misses rejected. Existing layout/sign-bit/age-65535 VM checks also passed.
- All 10 existing CUDA pipeline tests passed on hardware. `cargo check --release --locked`, formatting and diff whitespace checks passed. The test build reports an existing unused `benchmark_device` warning in `src/benchmark.rs`.

## Research and production boundary

[BCH's Schnorr specification](https://bitcoin-cash-node.gitlab.io/bchn-sw/bitcoincash-upgrade-specifications/2019-05-15-schnorr/) defines verification independently of nonce generation, including the R-y quadratic-residue rule; its nonce-generation section explains why predictable nonces compromise signing keys. [RFC 6979](https://www.rfc-editor.org/rfc/rfc6979) describes the deterministic construction removed by this experiment. Current pinned PHOTON covenant source `reference/vox/packages/photon/src/mine.cash` verifies the commitment signature, not its RFC6979 derivation; the VM evidence above verifies the experiment against the compiled covenant.

Here k is deliberately public and bounded to 1..2^32. Given k and a signature, d can be recovered from s=±k+e*d modulo n. This MUST NOT be used with a funded signing identity. `SearchHandle::start_inner` creates a search key independently of `RuntimeSupervisor::start_on_backend_device`'s intermediate reward identity (`new_intermediate_identity`). The live integration preserves that separation, rejects matching search/reward identities, and leaves reward signatures on RFC6979.

`verify_gpu_winner` reconstructs explicit-k signatures when the result contains `schnorr_k`, and RFC6979 signatures otherwise. Live CUDA integration is enabled only by the `incremental-k` build feature. HIP and WGPU retain their existing behavior. No AMD hardware result, long soak or independent block confirmation is claimed.

The final feature-enabled suite passed **257 tests, zero failures, three ignored**. The explicit hardware correctness run then passed all 13,920 candidates and full-batch samples, followed by the 216-sample VM checks above. The independent CPU oracle also matched the new production signature reconstruction. The new live-path check covers funded-identity rejection, boundary trimming, four baton ages, scalar 2^32, wrong-mode rejection, key rotation and rejection of a previous key's winner. `cargo check --locked --all-targets --all-features` and the feature-enabled release build passed. These checks ran before starting the sole live miner.

## Reproduce on this Windows machine

Run from the branch checkout in PowerShell, with the existing CUDA 13.4/VS2022 toolchain and libauth dependencies installed:

```powershell
cmd /c tools\build-incremental-k.cmd
cargo test --release --locked --target-dir D:\pickaxe-incremental-target incremental_k --no-run
New-Item -ItemType Directory -Force D:\pickaxe-incremental-target\release\deps\cuda\build | Out-Null
Copy-Item artifacts\incremental-k\ptx\*.ptx D:\pickaxe-incremental-target\release\deps\cuda\build
cargo test --release --locked --target-dir D:\pickaxe-incremental-target incremental_k_correctness -- --ignored --nocapture --test-threads=1
node tools\reward-policy-vm\photon-layout.mjs artifacts\incremental-k\vectors.json
cargo test --release --locked --target-dir D:\pickaxe-incremental-target incremental_k_benchmark -- --ignored --nocapture --test-threads=1
cargo test --release --locked --target-dir D:\pickaxe-incremental-target cuda_photon:: -- --test-threads=1
```

The adjacent test-executable PTX directory deliberately isolates kernel selection from `D:\pickaxe-live`. Experimental tests are ignored by default and fail if CUDA/PTX is unavailable. Keep GPU tests and timing runs serial; record concurrent GPU activity and thermals on each rerun.

Build the opt-in live executable with `cargo build --release --locked --features incremental-k --target-dir D:\pickaxe-incremental-target`. Deploy that executable with the six generated PTX files in its adjacent `cuda\build` directory, then use the normal `mine --backend cuda` TUI command with the existing payout. Stop any current miner before launching it. The checked live executable SHA-256 was `9B2C70AFD06457720CB3C3C65F2C1127FC0887B2FA1F3FFDACDD93F86B873AF3`.
