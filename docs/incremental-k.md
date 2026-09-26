# Incremental-k experiment — 2026-09-26

Branch `perf/incremental-k`, based on remote master/release commit `bb92508908b7a6e8235440168c6fe08140964890`.

**Result: 1.95x median complete-pipeline throughput**: 27.128 to 52.928 million candidates/second. This is an offline candidate, compiled only into the Rust test executable. No production switch, deployment, release, broadcast or master update was made.

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

Here k is deliberately public and bounded to 1..2^32. Given k and a signature, d can be recovered from s=±k+e*d modulo n. This MUST NOT be used with a funded signing identity. Source inspection confirms that `SearchHandle::start_inner` creates a search key independently of `RuntimeSupervisor::start_on_backend_device`'s intermediate reward identity (`new_intermediate_identity`); the experiment does not change or execute either live path. That separation must remain enforced in any future integration.

The production `verify_gpu_winner` still reconstructs RFC6979 signatures. It cannot consume these experimental scalar indexes as commitment nonces. A deployable version needs an explicit result representation, independent reconstruction of the chosen k signature, key/job generation binding and exhaustion handling, with the existing stale-job, settlement and donation checks preserved. No live winner, settlement, long soak or AMD result is claimed. This branch supplies the measured candidate and evidence for review before that integration.

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
