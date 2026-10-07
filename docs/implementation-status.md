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
| S1 | BCHN GBT-light/full template source and pinned submission | RPC fixtures, real Chipnet templates and accepted block | Real Chipnet full templates, confirmed ASIC blocks and independent public-server header checks pass; light/native TP pending |
| S2 | Knuth native Template Distribution client | Encrypted TP interoperability and fresh templates | Pending; Knuth live tests deferred, BCHN is the live-test baseline |
| S3 | Reference Noise, framing, setup, standard/extended mining channels | Reference device, tamper/replay/truncation and reconnect tests | Local encrypted TCP tests pass; upstream CPU device and ASIC pending |
| S4 | Per-device unique work, share validation, duplicate/stale rejection | Independent header oracle and reference CPU device | Header/merkle oracle, local CPU device and Avalon Nano 3 pass; refresh uniqueness follow-up and upstream native SV2 device validation tracked below |
| S5 | Submit valid blocks and report actual acceptance | Chipnet BCHN block acceptance/propagation | Durable full-block journal and outcome classification implemented; local lost-reply/write-failure/crash proof and physical ASIC block acceptance, public headers and clean restart recovery pass; evidence below |
| S6 | SV1 firmware translator into the same server | Reference translator and real user's ASIC | Reference adapter, CPU firmware TCP experiments and Avalon Nano 3 Chipnet blocks pass; retained-job fix deployed, refresh uniqueness follow-up below |
| S7 | Device dashboard rates, shares, rejects and reconnect state | Rendered TUI plus live devices | Per-session rows, validated-work estimates and SV1-local rejection/connection diagnostics implemented and host-tested; live telemetry and vardiff pending |
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

## Live BCHN preflight and CI ordering fix (2026-10-07)

After the owner's Start9 node came online, its authenticated private RPC
connection reported BCHN 29.1.0 on Chipnet, synced with peers. Pickaxe's
`stratum-v2 check-node` accepted a real full template for height 326691, with
bits `1d00ffff` and the node's 2,000,000-byte size limit.

The opt-in `live_chipnet_node_validates_standard_and_extended_block_proposals`
test then built standard and extended coinbases through the production channel
and template code. BCHN `validateblocktemplate` returned true for both complete
proposals at height 326712 (22 transactions including coinbase). An independent
decoder checked each merkle root and transaction count. Corrupting the merkle
root produced `bad-txnmrklroot` from BCHN's proposal API in both cases. No PoW was
searched and no block was submitted: this is real-node assembly validation,
not block acceptance, propagation or physical ASIC evidence.

Run that test only with `PICKAXE_CHIPNET_CONFIG` pointing to a private saved
Chipnet configuration, using `cargo test --locked --no-default-features
--features stratum-v2 --lib live_chipnet_node_validates_standard_and_extended_block_proposals
-- --ignored --nocapture`. It refuses other networks and does not use a wallet.
Node credentials, host details and the user's payout are outside the repository.

Windows CI on `101f444` exposed an acknowledgement/counter race: a peer could
receive its share ACK and the node could accept its block before the dashboard's
accepted-share counter was updated. The server now publishes validation counters
before sending the ACK. The TCP regression checks the counter immediately after
the peer receives it, without adding a sleep. All three server experiments pass
with four test threads; the full parallel host suite passes 371 library and 16
binary tests (17 ignored, 34 hardware-filtered). Formatting and Rust 1.99
all-feature Clippy with warnings denied pass. Exact pushed-commit CI is separate.

## Physical ASIC checkpoint and address-only worker (2026-10-07)

The Linux service built from `b3f3975586789e2a2ab33afbac1566712ef965b8`
is serving an Avalon Nano 3 running cgminer 4.11.1. An address alone in
`mining.authorize` succeeds without a worker suffix, including a real job.
The worker string remains an identity: this private server pays its configured
address, not an arbitrary address supplied by a connecting device. The actual
SV1 coinbase was independently decoded by BCHN and matched the configured payout.

Source-node block queries for Chipnet heights 326767 through 326770 returned
positive confirmations (4, 3, 3, 2 in sequential queries as the tip advanced).
Each coinbase paid the configured test address. This confirms real physical
ASIC mining and source-node chain inclusion; it does not prove independent
public-node propagation or complete the durable submission requirements.

The ASIC's cumulative rejection percentage fell from the owner's 27.7% startup
sample to about 5.0%. A subsequent 45-second passive server-to-ASIC sample saw
728 successful share responses and 8 rejections (1.087%): all eight were SV1
code 21, comprising five adapter `stale job` responses and three upstream
validator stale/invalid-job responses. No difficulty or duplicate rejection
was observed in that bounded sample. Firmware hardware-error counters are
separate and are not explained by this observation. No raw packets, device
credentials, payout addresses or network configuration are retained here.

At a later server snapshot, there were 57 accepted blocks and 130 unconfirmed
submission outcomes. The latter had stopped increasing during these samples,
but their causes are not yet classified; they must not be called accepted or
all described as node rejections. Current server counters also omit shares
rejected locally by the SV1 adapter, so aggregate server/firmware totals differ.

Static follow-up: every 15-second template refresh currently sends a future
job followed by `SetNewPrevHash`, and the adapter always sets `clean_jobs=true`,
including unchanged chain tips. The implementation therefore invalidates
in-flight work even on same-tip refreshes. A fix must retain and validate exact
same-tip job/transaction data while continuing to reject work for an obsolete
chain tip; do not lower rejection counts by accepting obsolete work blindly.
This behavior has not been changed in the running binary at this checkpoint.

Connections already have distinct session salts/extranonce allocations, even
when workers use the same address. Names are optional monitoring labels, not
the mechanism separating hashing work. The server and adapter each cap active
connections at 64; 1,000-ASIC capacity and persistent per-device labels are not
implemented or load-tested. Braiins Solo and CKPool both document address-only
usernames with optional worker suffixes:

- https://academy.braiins.com/braiins-pool/solo-mining
- https://solo.ckpool.org/
- Job lifecycle reference: https://stratumprotocol.org/specification/05-mining-protocol/

## Same-tip job retention fix (2026-10-07)

#### PR #38

Same-parent template updates now use immediately active SV2 jobs. The SV1
adapter translates them with `clean_jobs=false` and retains each job's version.
Both validators retain up to eight jobs with exact coinbase/template data;
new parents or changed bits still clear the old jobs. Duplicate protection
survives same-tip refreshes, including attempts to resubmit an identical header
under a different job identifier. The provider retains the corresponding exact
transaction lists for block submission. Existing 60-second ntime policy remains.

The TCP/Noise regression uses the actual 15-second server refresh. Two simulated
ASICs authorize with the same synthetic address and receive distinct extranonce
prefixes. A solution held through a same-tip refresh is accepted and submitted
as a full block checked by an independent decoder; the same job is rejected
after the next chain tip. Additional regressions cover bounded history, multiple
transaction-list refreshes, duplicate aliases, and standard/extended channels.

Local validation: 38 focused tests pass (one real-node test remains opt-in).
The all-feature host suite passes 375 library and 16 binary tests, with 18
ignored and 34 hardware tests filtered. Rust 1.99 formatting and all-target,
all-feature Clippy with warnings denied pass. No kernel or ASIC settings changed.
Exact-commit CI and updated physical-firmware observations are checked separately.
The durable block journal remains unfinished and is not included in this fix.

## Physical retention validation and unique refresh work (2026-10-07)

#### PR #38

The Linux binary for `5972a46` (SHA256
`3c3c9af1f4cc0d249f78f34aa6dacb03c7a94abca6f1be14baada9b83d3905f1`)
passed real-node preflight and replaced the previous service behind the same
ASIC endpoint. Its first 45-second passive sample saw 12 accepted responses,
no rejects, two retained-job notifications and one clean new-parent notification.
A later sample included a duplicate rejection; the initial clean sample is not
a claim of zero future rejects or a controlled rejection-rate comparison.

BCHN confirmed the updated service's block at height 326804 and the configured
payout matched its coinbase. Independently fetched headers for that height from
`chipnet.bch.ninja` and `chipnet.imaginary.cash` both hashed to the source node's
block hash. This establishes external visibility of that block, not universal
peer propagation. A subsequent service snapshot showed two accepted blocks,
zero unconfirmed outcomes and zero connection errors since restart.

A 120-second passive follow-up observed 29 submissions across eight same-tip
updates: 28 accepted and one duplicate (SV1 code 22). The duplicate repeated the
same coinbase/transaction commitment, extranonce, time, nonce and version under
a different job ID. The server correctly rejected the repeat, but reusing the
search space wastes work when firmware resets its nonce search on a new job.

The follow-up commits four job-ID bytes into the fixed coinbase prefix before
the session/channel and device extranonces. Standard and extended channels now
receive distinct work even when a refreshed template and share time are identical.
Device extranonce sizes and addresses remain unchanged. Retained old jobs still
validate against their own coinbase; actual repeated headers remain rejected.
This follows the SV2 [unique work requirement](https://stratumprotocol.org/specification/05-mining-protocol/).

The focused protocol suite passes 39 tests, with the real-node test opt-in.
That real-node test also passed for both standard and extended proposals at
Chipnet height 326808 (16 transactions): BCHN accepted each assembled proposal
and rejected a deliberately corrupted merkle root. It did not submit blocks.
The full host suite passes 375 library and 16 binary tests (18 ignored and
34 GPU tests filtered); formatting and Rust 1.99 all-target/all-feature Clippy
with warnings denied also pass. Exact-commit CI and deployment of the uniqueness
follow-up are recorded separately after completion. No raw packets, payouts or
credentials are included in this evidence.

## Physical unique-work validation (2026-10-07)

The Linux binary for `7ad0766` (SHA256
`49039505b4736dd81b68c11730dd8c544ff62041e96c0defb7e90d6f6fc77d73`)
replaced the service after real-node preflight, keeping the ASIC's pool address.
A 120-second passive sample observed 34 submitted and accepted shares, no
rejects or repeated work, four retained-job and four clean notifications.
BCHN confirmed heights 326809 and 326810 (two and one confirmations at the
checkpoint), with the configured payout in each coinbase. Both block headers
also matched responses from two independent public Chipnet servers.

There were zero unconfirmed block outcomes and no service restarts at the
checkpoint. Two connection errors still need classification; this is not a
claim of zero errors, universal propagation or a controlled rejection-rate
comparison. Only aggregate observations are retained.

## Durable full-block submission (2026-10-07)

#### PR #38

A device's solved full block is now flushed to a private, exclusively locked
journal before its share ACK. The saved state binds to the configured node,
network and payout, without storing RPC credentials. Lost replies remain pending
and retry the identical full block with bounded backoff. A current template is
never substituted for saved work. Exact BCHN success/duplicate responses or a
matching positively confirmed header establish acceptance; permanent validation
failures are separate from inconclusive results. A completion receipt and its
counter are committed with pending removal, preventing restart double counting.

Storage failure stops the service before ACK; corrupt/foreign state is preserved
and fails closed. Operational capacity is 64 pending blocks and 64 MiB raw data,
plus 1,024 recent receipts. These bounds are explicit rather than silently
dropping acknowledged work. Historical pre-journal outcomes are not imported.

Local TCP tests simulate a node accepting a block while all replies are lost,
then restart the server and verify byte-identical recovery and a single accepted
count. Another test forces an atomic-write failure: no share ACK or node
submission occurs. Journal tests cover locking, source/payout/network changes,
corruption, capacity and credential rotation. Retry timing is deterministic.
The full host suite passes 386 library and 16 binary tests, with 18 opt-in tests
ignored and 34 GPU tests filtered. An additional abrupt-exit test and final lint
checks also pass: the test subprocess exits without running destructors after
both a pending write and a completion receipt, and each state reopens intact.
Formatting and Rust 1.99 all-target/all-feature Clippy pass with warnings denied.
These are local experiments; the live
service at this checkpoint still runs `7ad0766`.

Node outcome semantics were checked against BCHN 29.1.0's
[submitblock implementation](https://github.com/bitcoin-cash-node/bitcoin-cash-node/blob/v29.1.0/src/rpc/mining.cpp)
and the [getblockheader RPC documentation](https://docs.bitcoincashnode.org/doc/json-rpc/getblockheader/).

## Physical durable-journal validation (2026-10-07)

The Linux binary for `d9df6ec` (SHA256
`7b47555521645fd1c2413e734a2411fbd0d5e0b7e7b2e9a77241bc4faf61ef6c`)
passed node preflight and 51 focused Linux tests. The dedicated Avalon Nano 3
mined heights 326820, 326821 and 326822 through it. Each configured payout
matched; source confirmations were 3/2/1, and headers matched independently
through both public Chipnet servers. A clean restart restored all three
completion receipts/counts with no pending blocks. The ASIC reconnected at its
unchanged endpoint and subsequently mined height 326823, advancing the saved
counter to four. The journal's permissions were 0600.

A 120-second passive sample had 19 accepted submissions, zero rejects and no
repeated work. Two connection errors accumulated before the controlled restart;
their causes were not available in that build. The live restart was graceful;
lost replies, failed writes and destructor-free exits were synthetic tests.
Exact-commit checks completed with 40 passes and release publication skipped.

## Per-device work and rejection accounting (2026-10-07)

#### PR #38

The TUI and JSON status now include a row per connected mining session. Generated
labels distinguish address-only workers; labels change on reconnect and are not
persistent physical-device identities. The local SV1 adapter and its native SV2
socket join the same row, so they do not count as two devices. No worker strings,
payouts or private socket addresses appear in these rows. Recent closed sessions
retain diagnostic reasons, with history bounded to 64 closed rows.

Hashrate is an estimate from accepted shares, weighted by each job's target:
`2^256 / (target + 1)` expected hashes per share. It uses a 30-second warm-up and
up to five minutes of wall time, decays during inactivity, and never reports a
device's advertised rate as measured work. Per-second aggregation bounds memory
independently of submission rate. Rejected/duplicate shares add no work.

SV1-local rejects now contribute to aggregate and per-device rejected totals,
before the response is sent, rather than disappearing outside the SV2 validator.
JSON distinguishes these from upstream rejects. Separate static SV1/SV2 error
categories identify where a session ended; one physical disconnect can affect
both transports, so those counters are not a unique-reconnect count. Normal
operator shutdown does not add connection errors.

Review of [SetTarget semantics](https://stratumprotocol.org/specification/05-mining-protocol/#5321-settarget-server---client)
also found active jobs using the latest channel target. Each installed job now
retains its original target for validation and work credit. Tests cover both
raising and lowering difficulty while an older job remains active.

Local validation passes 392 library and 16 binary tests, with 18 opt-in tests
ignored and 34 GPU tests filtered. This includes the two-device TCP experiment,
correct inclusion of SV1-local stale shares, weighted-rate/idle-decay checks,
bounded reconnect history, and rendered/scrolled 100- and 140-column dashboards.
Formatting and Rust 1.99 all-target/all-feature Clippy with warnings denied pass.
Live telemetry and exact pushed-commit CI are separate gates. Vardiff and greater
than 64 simultaneous devices remain unfinished; no 1,000-ASIC claim is made.
