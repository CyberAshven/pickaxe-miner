# Architecture implementation and evidence

Scope: all decisions, steps and follow-ups in `next-steps.md`, plus the SV2
design in `stratum-v2.md`. A scaffold or passing build is not completion.
Work continues on PR #38; this document does not narrow the requested scope.

## Invariants

- Mainnet and Chipnet share implementation; select network data explicitly.
- Retain native CUDA, HIP and portable GPU paths, payout/donation policy,
  reward-key separation, freshness checks and durable claims.
- Authenticate SV2 with the reference Noise/framing crates, including rigs.
- Only the coordinator owns chain access, payout configuration and claims.
- BCH jobs retain CTOR, omit witness commitments, use the node's current ASERT
  target and adaptive size limit, and validate payout/network compatibility.
- Treat credentials and private mining state as secrets. Keep logs redacted.
- No releases, tags or merges as part of this implementation.

## Requirements and completion evidence

| ID | Requirement | Evidence needed | Current state |
|---|---|---|---|
| S1 | BCHN GBT-light/full template source and pinned submission | RPC fixtures, real Chipnet templates and accepted block | Full-template fixtures pass; light/native TP and live proof pending |
| S2 | Knuth native Template Distribution client | Encrypted TP interoperability and fresh templates | Pending |
| S3 | Reference Noise, framing, setup, standard/extended mining channels | Reference device, tamper/replay/truncation and reconnect tests | Local encrypted TCP tests pass; upstream CPU device and ASIC pending |
| S4 | Per-device unique work, share validation, duplicate/stale rejection | Independent header oracle and reference CPU device | Header/merkle oracle and local CPU device pass; upstream/ASIC pending |
| S5 | Submit valid blocks and report actual acceptance | Chipnet BCHN block acceptance/propagation | Fixture acceptance/rejection distinguished; live proof and durable retry pending |
| S6 | SV1 firmware translator into the same server | Reference translator and real user's ASIC | Reference adapter and CPU firmware TCP experiments pass; physical firmware pending |
| S7 | Device dashboard rates, shares, rejects and reconnect state | Rendered TUI plus live devices | Aggregate dashboard added; per-device rate/vardiff and live validation pending |
| G1 | Coordinator CLI, payout, chain connections, claim journal and relay | End-to-end coordinator process tests | Pending |
| G2 | Rig CLI using every local GPU, pushed jobs and unique search keys | Multiple rigs/devices with independent winner verification | Pending |
| G3 | Coordinator re-verification, pause, durable claim and successor broadcast | Races, crashes/restart, stale winners and accepted claim | Pending |
| G4 | SV2 rig transport, backup coordinator failover, unified dashboard | Disconnect/failover tests without duplicate claims | Pending |
| D1 | P2Pool first-class/default destination beside own node | BCH sharechain interoperability and payouts | Pending |
| D2 | SV2 pool failover, own templates via Job Declaration, supported coinbase payouts | Compatible pool tests preserving CTOR | Pending |
| D3 | Guided local-node detection, cookies, name/version/sync and automatic fallback | Setup UI and connection/failure tests | Pending |
| T1 | ASIC-exclusive header jobs (SAFA-style) | Author's contract/deployment, VM proof, firmware and live test | Pending protocol/deployment evidence |
| T2 | BCH plus all compatible merge-mined tokens | Agreed covenant, coinbase commitment/merkle proof and VM/live proof | Pending author covenant design |
| F1 | Claim without waiting for slow GPUs | Review merged #35 and retain race/regression tests | Needs audit |
| F2 | Intel, native HIP discrete AMD, Linux GPU and Apple Silicon validation | Actual hardware reports; compile results are insufficient | Pending hardware evidence |
| F3 | Windows wgpu telemetry without vendor tools | Real portable-adapter metrics or explicit unavailable state | Pending audit |
| F4 | Sustained integrated + discrete GPU test | Long serial comparison, correctness and thermals | Pending |
| F5 | Next feature release version bump | Version/release plan approved for actual release | Deferred until release is ordered |

The owner has an ASIC and a Start9 BCHN Chipnet node available. Connection and
firmware details are being collected locally; they are not committed here.

## Initial audit (2026-10-07)

PR #38 head `0bd2a74` contains status/role constants and optional reference
dependencies only. Its Windows/Linux CI failures are `cargo fmt --check` errors.
Formatting is corrected locally. Eight scaffold tests passed with the SV2
feature enabled; they do not prove mining or transport functionality.

The existing generic node helper requests the Bitcoin `segwit` rule on GBT
fallback and can fall back from `submitblocklight` to full `submitblock` with a
light payload. The new SV2 path must avoid both assumptions and bind solved work
to the exact source/template that created it.

## Local implementation experiment (2026-10-07)

The generic full-template request no longer asks for SegWit. The new provider
exclusively uses full GBT plus full submitblock, with no light-payload fallback.
Its validation rejects wrong-chain/unsynced nodes, tip races, reordered or
duplicate transaction IDs, witness serialization and target mismatch.

The encrypted TCP experiment uses the production server and reference Noise,
framing and message codecs, plus a small CPU test device. Both standard and
extended channels submit two CPU-solved blocks and receive successor jobs on
the same connection. A separate oracle checks PoW, merkle root, coinbase
payout/value and serialization. Node rejection is explicitly tested: the
share ACK increments shares while block acceptance remains zero. These use a
synthetic easy target and a node fixture, not a real Chipnet block.

Additional tests cover pinned authority mismatch, tamper/replay/truncation,
fragmented reads, idle connections, duplicate/stale shares, version masks,
sequence wrap, extranonce separation, odd/even nonempty merkle trees and a
solution queued across a same-tip mempool refresh. Authority-file tests verify
identity reuse and fail-closed handling of a malformed saved key.

The local Start9 connection notes were located outside this repository. Both
SSH and the administration interface timed out, so no real node template,
accepted Chipnet block or propagation result is claimed. No credentials,
host details, payout addresses or local helper files are included here.

Known limits: the CLI uses a single selected full-template source; runtime
failover, GBT-light, native TP, block retry durability, vardiff, per-device
rates, distributed rigs, pool routing and BCH coinbase donation accounting are
not complete. Existing GPU/token runtime and donation policies are preserved.

Local validation for this checkpoint:

- `cargo fmt --check` passed.
- `cargo test --locked --no-default-features --features stratum-v2 --lib stratum -- --test-threads=1`: 28 passed.
- `cargo test --locked --all-features --no-fail-fast -- --test-threads=1 --skip if_cuda --skip if_hip --skip if_wgpu`: 364 library and 16 binary tests passed; 17 ignored and 34 hardware tests filtered.
- `cargo clippy --locked --all-targets --all-features -- -D warnings` passed.
- Native SV2 debug build and `stratum-v2 serve --help` passed. A build/CLI check does not validate the terminal on an attached ASIC.

Remote CI is checked on the pushed commit, independently of this local report.

## SV1 firmware experiment (2026-10-07)

The optional SV1 listener translates jobs and submissions through a pinned
Noise connection to the same SV2 server. SRI supplies the JSON protocol types,
job/difficulty conversion and extended-share conversion. The adapter retains
the server's coinbase and configured payout. Worker authorization registers
an identity; it is not password authentication. The plain listener is intended
for a trusted mining LAN and is disabled unless explicitly requested.

The independent CPU firmware experiment reconstructs headers from SV1 JSON,
including the previous-hash word byte order and extranonce split. It exercises
both subscribe/authorize orders and version rolling on/off. Each variant
submits two solved blocks, verified independently by the node fixture, then
receives successor work without reconnecting. This is synthetic easy-target
testing; it is not physical ASIC or real Chipnet acceptance evidence.

Boundary tests cover unauthorized/wrong workers, stale jobs, invalid extranonce
length, version-mask negotiation, pending-request bounds, malformed/oversized
and fragmented JSON, partial-input deadlines, and exact upstream reply mapping.
SV1 never acknowledges a share before the shared SV2 validator does. A separate
regression ensures share targets cannot hide easier network-valid block work,
including when Chipnet difficulty falls between jobs.

CI on `2c83810` passed the previous compiler lint failure. Its Linux dependency
policy check exposed the `hex_lit 0.1.1` MITNFA license, pulled by reference
Stratum dependencies. The unmodified compile-time macro's license was reviewed
against the SPDX text and allowed for that exact crate/version only. Local
`cargo deny check` passes advisories, bans, licenses and sources; no advisory or
source check was disabled. The separate GitHub advanced-security review run
failed with an account monthly-quota error, which is not a clean security result.

Local SV1 checkpoint: 35 focused tests pass; the full serial host suite passes
371 library and 16 binary tests, with 17 ignored and 34 hardware tests filtered.
All-feature Clippy passes with warnings denied on both the existing host
compiler and Rust 1.99 used by current CI. Formatting and `cargo deny check`
also pass. Remote checks are required on the subsequent pushed commit.
