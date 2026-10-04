# Browser T2 validation

#### PR #22

The portable engine reuses each BCH Schnorr signature across 65,536 token-amount
pairs, as the CUDA T2 engine does. Shared Rust checks transaction eligibility,
conserved amounts, recipient work allocation, fees and every returned winner.
The TypeScript interface only handles browser I/O and display. Browser intensity
starts at 50%, changes live, and displays a time-weighted rate once per second.

## Local evidence

RTX 5070 Ti Laptop GPU, NVIDIA driver 616.92, Brave WebGPU, intensity 100:

| Complete pipeline trial | Seconds | Million valid candidates/second |
| --- | ---: | ---: |
| 1 | 30.0139 | 778.128 |
| 2 | 30.0168 | 783.616 |
| 3 | 30.0004 | 771.182 |

These offline trials use the actual browser WASM and normal key and recipient
rotation. Work is counted once. They do not include live network waits and are
not a native CUDA speed claim. WASM SHA-256:
`4e53229e74aeb16d8beb65c2efb673378c09321f5e9387844e4073d7fa2117df`.

- 6,256 independent GPU signature, serialization and digest checks covered
  deployments, layouts, carries, partial windows and key/job rotation.
- A 4,194,432-candidate dispatch matched serial chunks and 1,076 independent
  winner reconstructions.
- Five synthetic T2 transactions passed BCH 2026 standard and consensus VM
  checks across recipients and layouts; tampered rewards were rejected.
- One full work cycle recorded 805,306,368 miner candidates and 16,777,216 for
  each donation recipient, preserving the compiled 96/2/2 allocation.
- Host-only suite: 264 passed, none failed, one ignored and 65 GPU tests filtered.
  Strict TypeScript and ten browser transport/submission checks passed.

Physical AMD and Apple performance comparisons remain separate requirements.
Mac compilation is not proof of Apple GPU mining.

## Mainnet acceptance and current dev validation

Source `ddb6159fed37fb16f86d445cfffc579257f5a469` mined one accepted
mainnet reward on 2026-10-04. Two independent Electrum servers returned the
exact submitted transaction. Independent Libauth BCH 2026 standard and
consensus VM checks passed, with the entered miner recipient and conserved
token reward verified; an edited reward failed. The transaction used one
input, two outputs and 629 sats for 629 bytes at the observed relay floor.
There was no extra funding input or split payment. The browser ended with
one accepted reward, no pending reward and no page errors.

The tested WASM SHA-256 was
`434a9966e8c096e42c7f6992c136bbb75a1bf45e8f69c27144cd9d3b24d0ca78`.
Three 30-second offline complete-pipeline trials on that build measured
910.305, 894.552 and 880.051 million candidates/second. These trials were
not interleaved with the older build, so they do not establish a speedup.
The actual full recipient work cycle and five independent covenant VM samples
also passed on this build. Shared GPU source migration remains unfinished;
see `tools/shared-gpu-proof/README.md` for the separate compiler proof.

Temporary local telemetry verified continued work while the page was hidden.
The telemetry hook is not included in distributed files. Browser discard and
operating-system sleep remain distinct from turning off the screen.

## Shared production build: mainnet reward

Commit `82f9ddda3154198d9a7664be8262c9b732fa0a82` subsequently mined an
accepted mainnet reward in Brave on the NVIDIA laptop. This build includes
the production shared Rust SHA/T2 filter and the live intensity/readout fix.
The WASM SHA-256 is
`b7f9bb3f8b91af2cffd942944a7cb6bc45b9d4c1ed1fd28de286f55e555d8e67`.

Two independent Fulcrum servers returned exactly the submitted bytes. An
independent BCH 2026 VM check confirmed both standard and consensus validity,
the entered payout, exact reward amount, one input, two outputs, and 629 sats
for 629 bytes. Increasing the token reward failed verification. The browser
stopped cleanly with one accepted reward, zero pending and no page errors.
Private payout and transaction fixtures are kept out of published evidence.

All 12 CI jobs passed at this commit, including native macOS ARM64 TUI checks
and compilation of the shared shaders with Apple's Metal compiler. Release
packaging passed with publication skipped. This is not a physical Mac mining
or performance result. The win occurred before the browser network-overlap
follow-up and must not be attributed to that transport change.

## Browser network refresh overlap

#### PR #22

The browser previously awaited three sequential server calls before issuing
more GPU work at every refresh. It now starts the next read while an existing
job is still fresh, then applies the completed response between GPU searches.
Requests stay sequential within each snapshot and there is only one snapshot
read in flight. The one-second freshness boundary still blocks additional
searches when a response is late. A winner drains any pre-winner read and starts
a new coherent read before submission. Stopping closes the connection and
drains the reader before freeing the Rust engine.

The protocol's [header subscription and token-aware UTXO methods](https://electrum-cash-protocol.readthedocs.io/en/latest/protocol-methods.html)
are unchanged. Both surrounding headers and all UTXOs still pass through the
same Rust parser and consistency checks; asynchronous callbacks never borrow
or mutate the WASM miner. No GPU kernel, payout policy or native TUI path changed.

Serial Brave/NVIDIA mainnet runs used the same server and WASM as `82f9ddd`,
100% intensity, 15 seconds of warmup and 60-second measurement windows. Three
ordinary baseline runs measured 715.719, 714.101 and 711.716 MH/s; three overlap
runs measured 1016.569, 1014.366 and 998.214 MH/s. Their means are 713.846 and
1009.716 MH/s, a local 41.45% improvement including network waiting. Time outside
GPU searches fell from about 19.5–19.7 seconds per minute to 0.89–0.91 seconds.

The raw sequence includes an initial exploratory run and a baseline run with
an accepted reward. Both were excluded from these means; a final baseline run
replaced the settlement-affected sample. Network latency and GPU boost behavior
can affect results. These are live control-loop measurements on one machine,
not a universal speed guarantee, native parity, or a 1.6 GH/s result. Raw counts,
durations and build hashes are in
[`network-overlap-measurements.json`](../tools/shared-gpu-proof/filter/network-overlap-measurements.json).

All runs stopped cleanly without pending rewards or page errors. Six new
regressions cover the freshness deadline, delivery between searches, a new
post-winner read, old responses after tab suspension, failed background reads,
and draining on stop/reconnect. All 16 browser tests, strict TypeScript and
the generated WASM controls/address smoke checks passed. The native TUI already
refreshes independently; its behavior and the native Mac TUI are unaffected.
