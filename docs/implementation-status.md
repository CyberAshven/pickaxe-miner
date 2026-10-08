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
| S3 | Reference Noise, framing, setup, standard/extended mining channels | Reference device, tamper/replay/truncation and reconnect tests | Local encrypted TCP tests, unmodified upstream standard-channel CPU device and SV1 Avalon Nano 3 pass; native SV2 firmware and external extended-channel coverage pending |
| S4 | Per-device unique work, share validation, duplicate/stale rejection | Independent header oracle and reference CPU device | Header/merkle oracle, local and upstream CPU devices, Avalon Nano 3 and refresh uniqueness pass; native SV2 firmware pending |
| S5 | Submit valid blocks and report actual acceptance | Chipnet BCHN block acceptance/propagation | Durable full-block journal and outcome classification implemented; local lost-reply/write-failure/crash proof and physical ASIC block acceptance, public headers and clean restart recovery pass; evidence below |
| S6 | SV1 firmware translator into the same server | Reference translator and real user's ASIC | Reference adapter, CPU firmware TCP experiments and Avalon Nano 3 Chipnet blocks pass; retained-job fix deployed, refresh uniqueness follow-up below |
| S7 | Device dashboard rates, shares, rejects and reconnect state | Rendered TUI plus live devices | Per-session rows, validated-work estimates and SV1-local diagnostics host-tested and observed on Avalon Nano 3; real brief RPC outage recovered without reconnect; workers page, read-only device reports and per-device vardiff host-tested, physical observation pending |
| S8 | Adjustable BCH donation with immutable job payouts | Arithmetic, journal recovery, independent wire/node checks and live payouts | Default 1.5% policy, dashboard controls and saved configuration implemented; host/reference checks and four live-node proposals pass; updated physical ASIC payout observation pending |
| G1 | Coordinator CLI, payout, chain connections, claim journal and relay | End-to-end coordinator process tests | `mine --rigs-listen`, or `--rigs-only` with no GPU; the normal miner's payout, chain connections, claim journal and relay; live two-rig Chipnet test below |
| G2 | Rig CLI using every local GPU, pushed jobs and unique search keys | Multiple rigs/devices with independent winner verification | `mine --coordinator`, every local GPU, its own search key; live with a CUDA rig and a wgpu rig below |
| G3 | Coordinator re-verification, pause, durable claim and successor broadcast | Races, crashes/restart, stale winners and accepted claim | 349 live rig winners checked and claimed with successor jobs, 3 stale, none rejected, 346 confirmed in blocks at the check; a coordinator crash during a claim is not exercised live |
| G4 | SV2 rig transport, backup coordinator failover, unified dashboard | Disconnect/failover tests without duplicate claims | Noise transport with the coordinator's pinned key, dashboard and JSON rig rows live; backup coordinators host-tested; live failover and rigs on separate machines pending |
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

## Physical telemetry and transient-template recovery (2026-10-07)

#### PR #38

The Linux telemetry binary for `2ee5347` (SHA256
`917d6f035e3fdeaa348d22babcdb2712445207d7f4a941565247e8f0869221ec`)
passed 56 focused Linux tests and node preflight. The Avalon Nano 3 appeared as
one SV1 device row at the unchanged endpoint. An initial 120-second passive
sample saw 25 submitted/accepted shares and no rejects. The service mined
height 326826; the configured payout matched and both independent public
Chipnet servers returned matching block headers. All seven existing/new journal
receipts were intact with no pending or rejected blocks at that checkpoint.

A longer four-minute observation ended at 111 accepted shares and one stale
rejection since restart. The stale share followed a new block and was correctly
included in the SV1-local and aggregate counters. It also captured a real
`template unavailable` disconnect, one unsuccessful reconnection and a successful
replacement session. The new session estimated 3.65 TH/s from 41 accepted shares
over about 198 seconds. This is statistical work accounting, not a controlled
performance comparison or proof of zero future connection errors.

The follow-up keeps authenticated device connections for at most three seconds
after template loss. All old jobs are revoked immediately; submissions during
the gap are rejected rather than credited or submitted to BCHN. Recovery sends
a clean activation on the existing channel, even for the same parent. Expired
30-second leases remain expired, repeated failures cannot extend the grace,
and longer outages still close the connection. Firmware can continue hashing
the revoked job briefly during this bounded gap; no pause capability is implied.
The TUI/JSON expose sanitized template-failure counts and the latest category.

The local TCP/Noise firmware regression holds a node outage, verifies an old
solution receives no ACK and no node submission, restores the node, rejects the
old job again and confirms a new block on the same session. Another regression
holds the outage beyond the grace and verifies disconnection. A deterministic
clock test covers lease expiry and repeated failures. These are synthetic
faults; no outage was forced on the owner's live node. Live deployment and
exact-commit CI for this follow-up are recorded separately.

The host suite passes 395 library and 16 binary tests (411 total), with 18
opt-in tests ignored and 34 GPU tests filtered. The 59 focused protocol tests,
formatting and all-target/all-feature Rust 1.99 Clippy also pass. No GPU kernel,
device firmware, clock setting, permanent endpoint or payout policy changed.

The deployed `67e7774` Linux executable (SHA256
`9785379b8e5c012a431f2fbda840ff213ea0baaea7df4456831b79e7a7a1df33`)
passed preflight and preserved ten receipts during a clean replacement. A
six-minute observation recorded 97 accepted shares, one rejected old share and
one real RPC failure, followed by healthy work on the same session with zero
SV1/SV2 connection errors. No outage was forced and no block was found in that
bounded sample. Subsequent observation recorded four accepted submissions at
height 326830: one had a confirmation and three were on losing branches. Saved
acceptance receipts record node acceptance, not permanent canonical rewards.
All 40 exact-commit checks for `67e7774` passed; release publication was skipped.

## Independent native SV2 reference device (2026-10-07)

#### PR #38

An unmodified [SRI mining_device](https://github.com/stratum-mining/sv2-apps/tree/3772e9d890dc7ec23bc82bcead41879542902402/integration-tests)
was built from commit `3772e9d890dc7ec23bc82bcead41879542902402` with Rust
1.98.1 on Debian 13. Its locked Stratum core revision is
`c293d7418a07d2bf510496870c03e6dc0683db8a`; the upstream Cargo.lock SHA256 is
`861a6f0cef29b7c69f164f2721ae5d78beaf8701180efee6e0abf26f989fc093`.
The local reference executable SHA256 is
`d0ded488f854de0bcaf019053dd2a39a28911c409a503fabfdb4b323a1d1c3d9`.
No upstream source or dependency lockfile was modified or copied into Pickaxe.

The opt-in `upstream_reference_device_authenticates_and_mines_successor_blocks`
test runs the real production server against synthetic BCH node fixtures. The
independent client first rejects the wrong pinned authority without submitting
work. With the correct key, it opens a standard channel and solves two valid
successor blocks on one connection. The fixture separately decodes each block,
checking its PoW, parent, merkle root, coinbase value and configured payout, and
BCH serialization. One CPU thread is throttled to one nonce per second; neither
the live node, the ASIC endpoint nor a GPU is involved. The initial Linux
experiment passed in 32.63 seconds.

CI now builds that exact reference revision with locked dependencies and runs
this test separately from the ordinary host suite. The test remains ignored
unless explicitly selected with `PICKAXE_SV2_REFERENCE_DEVICE` set to the
independently built executable. The source is MIT OR Apache-2.0; this experiment
does not incorporate it into the shipped miner. Native Bitaxe firmware,
external extended-channel clients and other upstream pools remain unproven.

Review against [Mining Protocol section 5.3.1](https://stratumprotocol.org/specification/05-mining-protocol/#531-setupconnection-flags-for-mining-protocol)
also corrected the adapter's setup-success flags: bit 0 requires a fixed
version, and bit 1 requires extended channels. The adapter accepts the extended
requirement, rejects fixed-version or unknown requirements, and validates the
response version and frame header. Its local upstream sends flags zero, so
this correction does not explain earlier physical ASIC rejects. A table-driven
test covers both requirements and malformed negotiation variants.

After the correction, the Linux protocol suite passed 59 tests (two opt-in tests
ignored), and the independent reference experiment passed again in 21.63 seconds.
The Windows all-feature suite passed 399 library and 16 binary tests; 19 opt-in
tests were ignored and 31 CUDA tests filtered. Formatting and Rust 1.99
all-target/all-feature Clippy passed with warnings denied. Remote CI for the
new commit is a separate gate; the live ASIC service still runs `67e7774`.

To reproduce on Linux, install `capnproto`, `libcapnp-dev`, `libssl-dev` and
`pkg-config`, then build the pinned client in a separate checkout:

```sh
rustup toolchain install 1.98.1 --profile minimal
git clone https://github.com/stratum-mining/sv2-apps.git ../sv2-reference
git -C ../sv2-reference checkout --detach 3772e9d890dc7ec23bc82bcead41879542902402
CARGO_PROFILE_DEV_DEBUG=0 cargo +1.98.1 build --locked --manifest-path ../sv2-reference/Cargo.toml -p integration_tests_sv2 --bin mining_device --jobs 2
PICKAXE_SV2_REFERENCE_DEVICE="$(realpath ../sv2-reference/target/debug/mining_device)" cargo +1.98.1 test --locked --no-default-features --features stratum-v2 --lib upstream_reference_device_authenticates_and_mines_successor_blocks -- --ignored --test-threads=1
```

If a shared `CARGO_TARGET_DIR` is configured, use the executable in that target
directory instead. Successful synthetic interoperability is not a live block
propagation or hardware performance claim.

## BCH donation checkpoint (2026-10-07)

#### PR #38

The BCH server now shows one adjustable donation percentage, starting at 1.5%.
The internal calculation compensates for the overlap between donation work and
personal rewards so the expected combined percentage targets the selected total.
It does not round up a satoshi charge: fractional satoshis remain with the miner.
The arithmetic test checks every supported setting from 1.50% through 100.00%,
and the existing PHOTON policy is unchanged.

The CLI accepts `--donation`; dashboard `+` / `-` controls save changes before
applying them to new jobs. A separate wire job ID commits fresh search space on
policy rotations without changing the node's template generation. Retained jobs
keep their original payout plan. The journal validates that plan on recovery and
replays pre-donation pending blocks byte-for-byte. The work adapter's timing and
firmware limits are described in `stratum-v2.md`; rewards are not guaranteed to
match the percentage over a short session.

Validation of this source checkpoint:

- Windows Rust 1.99 all-feature host suite: 407 library and 16 binary tests passed,
  19 opt-in tests ignored, 34 hardware tests filtered. One earlier full run hit a
  loopback Noise handshake timeout; the focused retry and complete suite passed
  without changing or extending production timeouts. That transient is recorded,
  not treated as a proven transport fix.
- Formatting and all-target/all-feature Clippy with warnings denied passed.
- Linux Rust 1.98.1: 66 protocol tests and three BCH arithmetic tests passed.
- Unmodified pinned SRI reference device: authority rejection and two successor
  blocks passed in 19.63 seconds with independently decoded payout outputs.
- Real BCHN 29.1.0 Chipnet proposals: standard and extended personal/donation-work
  coinbases all validated at height 326846, each with 16 transactions; all four
  corrupted-merkle controls were rejected. No block was broadcast by this test.
- Test-source archive SHA256:
  `1386b6e8bd657e86af8749d69ac35c4a32c5f653b133b02eba2e2f33d09c567a`.

At this checkpoint the physical ASIC service remains on `67e7774`; new physical
payout evidence and exact pushed-commit CI must be recorded separately. This
checkpoint does not complete the remaining requirements in the matrix above.

## Workers page, device reports and vardiff checkpoint (2026-10-08)

#### PR #38

This checkpoint follows the operator's review of the Avalon Nano 3 service.

- Donation: the setting lives in Advanced settings (`a`), from 0% to 100% in
  0.5% steps, default 1.5%. One third is mining work and two thirds block
  reward, each rounded up to two decimals when shown; this replaces the earlier
  formula that compensated for their overlap. Mainnet and Chipnet each donate
  to their own built-in address through the shared payout validation.
- Workers page: the default dashboard page lists each worker with 5-minute and
  1-hour rates, rejects, last share, difficulty and the device's own report;
  Tab switches to the overview.
- Device reports: read-only CGMiner API (`summary`, `estats`, port 4028) and
  Bitaxe `/api/system/info` queries every 15 seconds, to local-network
  addresses only, three seconds per device. Addresses never reach the display
  or JSON.
- Vardiff: SRI's reference rules per channel, about 20 shares a minute,
  starting at 4096 with a 1 MH/s floor; a new target applies with a fresh job.
- Chipnet difficulty-1 windows: the device target now returns after following
  an easier block target. It previously stayed down until reconnect, flooding
  shares (one Avalon Nano 3 reported 260,039 accepted shares at an average
  difficulty of 85). Only the first solved block per parent is saved, so such a
  burst can no longer fill the 64-block journal and stop the server.

Validation: Windows all-feature host suite, 426 library and 16 binary tests
passed, 19 opt-in tests ignored, 34 hardware tests filtered; formatting and
all-target/all-feature Clippy with warnings denied passed. Vardiff convergence
is simulated for 1 TH/s, 4 TH/s, 90 TH/s, 200 TH/s and 1 PH/s devices. Physical
observation on the Avalon Nano 3 and exact pushed-commit CI are separate gates.

## GPU farm: a coordinator with no GPU and two live rigs (2026-10-08)

#### PR #32

`mine --rigs-listen ADDR --rigs-only --address PAYOUT` runs a coordinator
that uses no GPU on its computer: a search with no GPU follows each job's
generation and pause state and finds nothing, the rigs mine, and the claim
path is the normal miner's. `--rig-name` names a rig (default: the computer's
name, with `/etc/hostname` for a Linux service), a rig prints its GPUs at
start, a headless rig (`--no-tui`, `--json`) no longer asks for `--address`,
rigs measure their own rate (the rate they sent was a field only the main
miner's runtime fills, so it read 0), a coordinator takes up to 1,024 rigs,
and the coordinator's Hashrate row and chart add the rigs' rate.
`docs/farm.md` is the operator guide.

Live on Chipnet, on one PC, with this branch's release build: the coordinator
(`--rigs-only`) and two rigs, rig A on the RTX 5070 Ti Laptop GPU through CUDA
and rig B on the AMD Radeon integrated GPU through wgpu (per-process GPU
counters showed them on different adapters), each process with its own session
folder; the operator's mainnet miner was paused between claims for the 10
minutes and restarted afterwards. In 10 minutes the rigs sent 357 winners (rig
A 347, rig B 10). The coordinator checked and claimed 349 of them, each
followed by the next job on both rigs; 3 were stale (found after their job had
moved on), none was rejected, and the last few were not claimed before the
test stopped. A Chipnet Fulcrum server then had 346 of the 349 claims in two
blocks and the other 3 in its mempool. Neither rig reconnected, and the
coordinator's process showed no GPU activity. That build reported each rig's
rate as 0; rigs now measure their own rate (fixed after the test,
host-tested).

Validation: Windows all-feature host suite, 438 library and 16 binary tests
passed, 22 opt-in tests ignored, 34 hardware tests filtered; new tests cover
the search with no GPU, the rig's rate window, the coordinator's dashboard with
no GPU and the CLI options. Formatting and Clippy with warnings denied passed
for the all-feature, default and portable builds, and the all-feature Clippy
on Rust 1.99 (Linux, in Docker).

## Public GPU pool (2026-10-08)

#### PR #32

`mine --rigs-listen ADDR --rigs-public [--rigs-fee P --rigs-fee-address A]`
makes the coordinator a public GPU pool. A rig sends its payout with its
hello (`--address`); a public coordinator turns away a rig without a valid
one and gives each rig the shared job paying its own payout. For the
operator's fee, a 10-minute clock per rig gives that share of its time to the
same job paying the fee address, marked by the top bit of the job's
generation, so the rig takes it as a new job; the rig's donation still
applies inside every job, so the fee comes off what the donation leaves.
Each winner is checked against exactly the job its rig was given, then
queued under the shared generation with that job's payout, and the claim
path authorizes that payout (`VerifiedWinner::payout`) instead of the
coordinator's own; a winner with no payout is checked against the
coordinator's payout as before.

Evidence: host tests cover the job variants and the fee clock; a loopback
coordinator over the encrypted link turns away a rig that names no payout,
sends a rig its fee-window job, queues that job's winner under the shared
generation for the fee address, and sends the rig's own job outside the
window; the direct-reward lifecycle refuses a winner paying another address
unless the coordinator vouches for it. Not yet run with rigs on other
machines. A PHOTON claim can be broadcast by anyone, so the fee is
voluntary for a modified rig, as the donation is.
