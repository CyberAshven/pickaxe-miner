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
| S7 | Device dashboard rates, shares, rejects and reconnect state | Rendered TUI plus live devices | Per-session rows, validated-work estimates and SV1-local diagnostics host-tested and observed on Avalon Nano 3; real brief RPC outage recovered without reconnect; workers page, read-only device reports and per-device vardiff observed on the Avalon Nano 3 on 2026-10-08; the read-only `watch` view and calmer vardiff host-tested |
| S8 | Adjustable BCH donation with immutable job payouts | Arithmetic, journal recovery, independent wire/node checks and live payouts | Default 1.5% policy, dashboard controls and saved configuration implemented; host/reference checks and four live-node proposals pass; updated physical ASIC payout observation pending |
| G1 | Coordinator CLI, payout, chain connections, claim journal and relay | End-to-end coordinator process tests | `mine --rigs-listen`, or `--rigs-only` with no GPU; the normal miner's payout, chain connections, claim journal and relay; live two-rig Chipnet test below |
| G2 | Rig CLI using every local GPU, pushed jobs and unique search keys | Multiple rigs/devices with independent winner verification | `mine --coordinator`, every local GPU, its own search key; live with a CUDA rig and a wgpu rig below |
| G3 | Coordinator re-verification, pause, durable claim and successor broadcast | Races, crashes/restart, stale winners and accepted claim | 349 live rig winners checked and claimed with successor jobs, 3 stale, none rejected, 346 confirmed in blocks at the check; a coordinator crash during a claim is not exercised live |
| G4 | SV2 rig transport, backup coordinator failover, unified dashboard | Disconnect/failover tests without duplicate claims | Noise transport with the coordinator's pinned key, dashboard and JSON rig rows live; backup coordinators host-tested; live failover and rigs on separate machines pending |
| D1 | P2Pool first-class/default destination beside own node | BCH sharechain interoperability and payouts | Pending |
| D2 | SV2 pool failover, own templates via Job Declaration, supported coinbase payouts | Compatible pool tests preserving CTOR | SV1 devices at SV2 pools with backup pools in order implemented and tested (pool mode below); Job Declaration in both modes, Pickaxe client to Pickaxe pool, implemented and loopback-tested (below); live Job Declaration runs and coinbase payouts at other pools pending |
| D3 | Guided local-node detection, cookies, name/version/sync and automatic fallback | Setup UI and connection/failure tests | Setup finds a BCHN on this computer and offers it with client, version and sync height; cookie login; unusable nodes named with their reason; live-checked against a throwaway Chipnet BCHN (below). GPU broadcasts already fall back to Fulcrum; ASIC mode has no fallback, since only a node supplies full templates |
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

## Workers view and calmer vardiff checkpoint (2026-10-08)

#### PR #40

Live evidence before this change, from the Avalon Nano 3 on the Chipnet
service running PR #38's head (`03135d3`), about ten minutes after the
restart: the device's own report (3.92 TH/s five-minute rate, 94 °C hottest,
fan 73%) beside Pickaxe's measured 4.47 TH/s over five minutes and 4.31 TH/s
since the restart; 93 accepted shares and one stale; the donation at 1.50%;
three brief node RPC failures recovered without dropping the device. Vardiff
had moved the difficulty from 4096 to about 5,297, above the expected 2,800 to
3,100: SRI's rules alone can act on a short run of lucky shares, the likely
cause of that overshoot.

This change adds `stratum-v2 watch`, a read-only workers table for a server
running without a screen, fed by a status file the server saves each second
beside its config, and makes vardiff calmer: it acts only on 72 shares, four
minutes or a silent minute, and ignores changes under 25%, as ckpool and
P2Poolv2 do.

Validation: Windows all-feature host suite, 429 library and 16 binary tests
passed, 19 opt-in tests ignored, 34 hardware tests filtered; formatting and
all-target/all-feature Clippy with warnings denied passed. Physical
observation of the calmer vardiff and the `watch` view on the Avalon's service
is a separate gate.

## Device reports and controls through asic-rs (2026-10-08)

#### PR #40

Device reports and controls now come from asic-rs 0.8.5 (256 Foundation,
Apache-2.0), which identifies most SHA-256 makes and firmwares, mixed with
Pickaxe's own CGMiner and Bitaxe code as the extension (`fleet.rs`). asic-rs
supplies model, firmware, power and the reports of makes Pickaxe did not read;
Pickaxe's reader keeps the 5-minute rate, the hottest reading and the fan
where a device gives them, reads Avalon Nano power, answers for unidentified
devices, and sends Avalon work levels. Controls list what asic-rs supports
for the device (Restart, Pause, Resume, blink the light) plus Avalon work
levels. Pool settings, MAC addresses, serial numbers and host names are never
collected. All connected devices are asked in parallel (up to 32 at once).

Live, read-only check against the operator's Avalon Nano 3 on the local
network (opt-in test `real_device_is_identified_and_reported`): asic-rs
identified "AvalonNano3", firmware 25103101_0736b2e, and offered Restart,
Pause, Resume and both light actions; the mixed report showed 3.42 TH/s
(five-minute), 95 °C hottest, fan 74% and 127 W. asic-rs alone read the
Nano's power as 2,756 W: the Nano's `PS[0 0 0 4 2756 126 330]` holds its
27.56 V input fifth and watts sixth, while asic-rs reads the fifth value as
watts for every Avalon. Pickaxe reads Nano power itself; this is worth
reporting to asic-rs.

Dependencies: asic-rs brings tokio, reqwest with rustls and the makes and
firmwares crates; it builds on Windows and Linux. cargo-deny (advisories,
bans, licenses, sources) passes with one scoped exception: webpki-root-certs
1.0.9, Mozilla's root certificates as data under CDLA-Permissive-2.0.

Validation: Windows all-feature host suite, 438 library and 16 binary tests
passed, 20 opt-in tests ignored, 31 hardware tests filtered; formatting,
all-target/all-feature Clippy and the server-only (`stratum-v2`) Clippy with
warnings denied passed. A device control action was not sent in this check.

## Pool mode: SV1 devices at a remote SV2 pool (2026-10-08)

#### PR #40

`stratum-v2 serve --upstream HOST:PORT --upstream-key KEY [--upstream-user ID]`
runs only the SV1 adapter, pointed at a remote SV2 pool: no node and no local
SV2 server. Each device gets an encrypted, key-pinned SV2 connection and an
extended channel under the identity (default: the payout address, never
printed). Pools may batch acknowledgements, so the adapter answers firmware
once a share is checked and forwarded and counts the pool's verdicts itself;
a changed extranonce, a closed channel or a Reconnect closes the device for a
fresh channel. Local mode keeps its exact behavior: per-share
acknowledgements from Pickaxe's own server before firmware gets its reply.
The donation does not apply in pool mode, since the pool builds the blocks
(changed the same day: see "Pool mode: the donation as mining time").
Backup pools repeat `--upstream` and `--upstream-key` in order: each device
takes the first pool that completes the handshake, setup and channel, and a
device whose pools all fail keeps a row with the last pool's reason.

Evidence: an end-to-end test with Pickaxe's own server as the pool, reached
by host name over Noise after a first pool with the wrong key is skipped,
mines two blocks with SV1 firmware, and the adapter's separate statistics
count both verdicts. Read-only against SoloFury's BTC SV2
(`eu-btc.solofury.com:3333`) with a throwaway identity, an SV1 device stand-in
got a 4-byte extranonce prefix, an 8-byte extranonce2, difficulty 1024 and a
first job (opt-in test `real_sv2_pool_sends_work_to_sv1_firmware`). SoloFury's
BCH SV2 endpoints send a Noise certificate with format version 1 (CashStratum
`src/sv2_noise.c` sets it); the SV2 spec requires 0 and requires clients to
refuse other versions, as SRI's `noise_sv2` 2.0 does, so Pickaxe names that
reason on the workers page. Reported as
[cashstratum/cashstratum#3](https://github.com/cashstratum/cashstratum/issues/3).

Validation: Windows all-feature host suite, 443 library and 16 binary tests
passed, 21 opt-in tests ignored, 31 hardware tests filtered; formatting,
all-target/all-feature Clippy and the server-only Clippy with warnings
denied passed.

## Own BCH node: found on this computer, cookie login (2026-10-08)

#### PR #40

The guided own-node step (D3). The BCH node list in setup looks for Bitcoin
Cash Node on this computer in the background, at BCHN's default RPC address
(`127.0.0.1:8332` on mainnet, `127.0.0.1:48332` on Chipnet, from its
`chainparamsbase.cpp`), and offers it with its client, version and sync
height; Enter saves it for every profile on that network. A node that wants
a login, a node on the other network and no node at all are each explained,
with what makes BCHN answer (`server=1`, plus `chipnet=1` for Chipnet).
Start in ASIC mode without a node opens this list directly.

With no `rpcpassword` set, BCHN writes `__cookie__:<hex>` to `.cookie` in its
network's data folder at every start (`rpc/protocol.cpp`). Pickaxe reads it
for every call, only for a loopback host on those default ports, from BCHN's
default data folder (`util/system.cpp`) and on Linux also its service folder
`/var/lib/bitcoind` (`contrib/init/bitcoind.service`);
`PICKAXE_NODE_RPC_COOKIE` names another file. A login in the URL or the
environment still comes first. The cookie is never printed or saved.

A node's HTTP 401 is now named ("the node refused the RPC login") instead of
a JSON parse error, and `check-node` and `serve` say why no configured node
could be used (a refused login, the wrong network, not synchronized).
`check-node`, the dashboard header and the status `watch` reads show the
node's client and version, such as "Node Ready (Bitcoin Cash Node 29.1.0)".

Fallback: GPU broadcasts already fall back from a node to Fulcrum servers.
ASIC mode has none, since only a node supplies full block templates; saved
nodes are tried in order at start.

Evidence: host tests cover the cookie paths per network and host (other hosts
and ports get none), login precedence, the 401 mapping, the node report and
its summary, user agents with control characters, saved forms of the local
address, and the setup flow (the offer, Enter saving it, the saved state, the
other network, a login wanted, no node). Live, against a throwaway BCHN 29.1.0
Chipnet container with no peers and no password, reachable from this computer
only: the opt-in `real_local_node_is_found_and_reported` read
"Bitcoin Cash Node 29.1.0 · syncing, height 0 of 0 (0%)" with its cookie and
`NeedsLogin` without it, and `check-node` named "node not synchronized" and
"node refused the RPC login" respectively. No synchronized node on this
computer was available, so a synced node in the offer is host-tested only.

Validation: Windows all-feature host suite, 444 library and 16 binary tests
passed, 22 opt-in tests ignored, 34 hardware tests filtered (one run also saw
the timing-sensitive `sv1_same_tip_refresh_accepts_inflight_block_and_new_tip_rejects_it`
time out under parallel load; it passed alone three times and does not use
the node code changed here); formatting, all-target/all-feature Clippy on Rust
1.94 and 1.99 and the server-only Clippy with warnings denied passed.

## Windows: accepted sockets block again (2026-10-08)

#### PR #40

The release-mode test run on GitHub's Windows runner failed in a server test
whose simulated firmware lost its connection before its first job. Locally,
two or three of every five or six release-mode runs of the server tests
failed the same way, in different tests. The cause: on Windows an accepted
socket inherits its listener's non-blocking mode (Linux does not), and both
the SV2 server and the SV1 adapter accept on non-blocking listeners. The SV2
server's Noise handshake then read at once, and when the client's first bytes
came a moment after the connection it failed and dropped the client; the SV1
adapter's reads and writes likewise failed instead of waiting out their
timeouts. Both now set each accepted socket blocking before using it. A
Windows-hosted server could drop SV2 devices this way; the physical ASIC runs
against the Linux service, which is unaffected.

Validation: eight release-mode runs of the server tests with 16 threads
passed (two or three of every five or six failed before); the Windows
all-feature host suite passed 444 library and 16 binary tests; formatting,
all-feature Clippy and server-only Clippy with warnings denied passed.

## Pool mode: the donation as mining time (2026-10-08)

#### PR #40

The operator decided the donation applies however Pickaxe is used, including
at a remote pool, with the BCH setting's 0% option kept. A pool builds its own
blocks, so the whole BCH donation is mining time there: for that share of each
device's time (9 seconds of every 10 minutes at the 1.5% default), the device
mines at the same pool under the network's donation address, on a second
extended channel of the same encrypted connection. The schedule is the
server's own per-device work clock, counted while the device has work. At 0%
no donation channel is opened.

To switch channels without reconnecting the device (which could make it fall
back to its own backup pools), the adapter now owns a pool-mode device's
extranonce: extranonce1 is four bytes it picks and extranonce2 the four the
device rolls, together the channel's eight miner bytes; the pool's channel
prefix, and zero padding if a pool grants more than eight bytes, goes into the
coinbase part the device receives. A switch sends the channel's difficulty and
a clean job; the donation channel's jobs carry the top bit in their SV1 job
number, so each share returns to the channel its job came from, with its
extranonce rebuilt. A pool that refuses or later closes or changes the
donation channel leaves the device mining on its own channel and puts the
reason on its workers-page row. Local mode (this server's own listener, which
the Avalon Nano uses) is unchanged. `--donation` now works with `--upstream`.

Evidence: unit tests cover the extranonce split and coinbase prefix, opening
the donation channel only once the device is ready, the switch at 100% with a
clean job of donation number and padding for a ten-byte grant, a donation
share's channel, job and extranonce, the switch back at 0%, and a refused or
closed donation channel. End to end, with Pickaxe's own server as the pool
over Noise, SV1 firmware at a 100% donation mined a block from a donation
channel job; the pool validated the share and the node fixture accepted the
block, which checks the coinbase the device built from the adapter's
extranonce. The existing pool test (two blocks, verdicts counted, fallback
past a wrong key) passes with the new extranonce split. Not yet run against a
real BCH SV2 pool: SoloFury's BCH endpoints still send the certificate version
reported in cashstratum/cashstratum#3.

Validation: Windows all-feature host suite, 447 library and 16 binary tests
passed, 22 opt-in tests ignored, 34 hardware tests filtered; formatting,
all-target/all-feature Clippy on Rust 1.94 and 1.99 and the server-only Clippy
with warnings denied passed.

## Public pool: each miner paid at their own address (2026-10-08)

#### PR #40

`stratum-v2 serve --public [--pool-fee P --pool-fee-mode coinbase|work|both
--pool-fee-address A]` runs the ASIC server as a public pool
([pool.md](pool.md)). Each channel pays the payout its user names (an
address, with or without its network prefix, optionally followed by
`.worker`); a name that is not a payout on the pool's network gets the SV2
`unknown-user` error, which SV1 firmware sees as a refused authorize. The
coinbase pays the miner, then the Pickaxe donation (first and in full), then
the operator's fee from what the donation leaves: from the coinbase, from
mining work (the server's work clock gives the operator's slice right after
the donation's), or both (a third work, two thirds coinbase). No custody:
nothing is paid out later. The dashboard header names the public pool and
its fee.

SV1 firmware cannot give its username before its subscription is answered,
so in a public pool the adapter answers the subscription with an extranonce
of its own (as at a remote pool), opens the device's channel only at
authorize under the device's username, and answers the authorize when the
server accepts or refuses that name. The block journal records the miner and
the fee address of a block that does not pay the configured payout and
checks every pending block against them on restart. Coinbase payouts, the
fee address and pool miners' addresses now accept P2SH (`p`, such as a
multisig, with a 20-byte or 32-byte hash) as well as P2PKH; PHOTON payouts
stay P2PKH.

Evidence: end to end with this server's own node fixture, two SV2 devices
(a standard and an extended channel, one with a bare address and a worker
name) each mined a block that paid exactly their address, the donation's 1%
and a 2% coinbase fee of the remainder; through the SV1 adapter, two SV1
devices with their own addresses were each paid by their own block with a
1% fee, and a non-address username was refused at authorize. Unit tests cover
the fee arithmetic in each mode (the donation first), old journal records
without a fee, usernames with and without prefixes and workers, P2SH and
P2SH32 scripts, a journal restart with a public pool's block and a tampered
miner failing closed, the adapter's held authorize, and the new options. Not
yet run with real public miners.

Validation: Windows all-feature host suite, 455 library and 16 binary tests
passed, 22 opt-in tests ignored, 34 hardware tests filtered; formatting,
all-target/all-feature Clippy on Rust 1.94 and 1.99 and the server-only Clippy
with warnings denied passed.

## Setup: solo, join a pool, or run one (2026-10-08)

#### PR #40

The setup's first screen now offers GPU mining, ASIC mining and Run a pool.
ASIC mining's settings page has a Mining row: solo on your node, or join a
pool, which asks for the pool type (a normal pool; P2Pool v2 is listed as
coming), the pool's `HOST:PORT` and its authority key, and starts pool mode.
Run a pool asks what miners mine (an ASIC pool; a GPU pool needs the GPU farm
of PR #32 in the same build), the pool type, the payout address and node, the
pool fee in 0.5% steps, where it comes from (coinbase, mining work, or both)
and its address (q or p, checked for the network), and starts the public pool.
Host tests walk each path, including the refusals for P2Pool v2, a GPU pool
in this build, a missing pool address or key, a bad fee address and a missing
node. Pool settings last for the session, as the hardware choice does.

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

## Connection info for rigs (2026-10-08)

#### PR #40

The coordinator's dashboard has a Connection info page (`I`): the exact
`pickaxe mine --coordinator ADDRESS --coordinator-key KEY` command a rig runs
(a public pool adds `--address YOUR_BCH_ADDRESS`), once per address other
computers can use, with a number key to copy each. `src/reach.rs` finds the
addresses: a wildcard listener is shown at this computer's address on the
local network and, when Tailscale is up, its Tailscale address (100.64.0.0/10),
found by asking the system which interface it would send from (a connected UDP
socket; nothing is sent and no outside service is asked). A loopback listener
is shown as reachable from this computer only. Copying uses the terminal's
OSC 52 (which also reaches an SSH client's clipboard) and the system's own
tool where there is one (`clip` on Windows, `pbcopy` on macOS). Without a
Tailscale address the page suggests Tailscale for rigs in other places. The
`rigs` start line lists the addresses under `connect`; headless text mode
prints each command as a `rigs join` line.

Evidence: host tests cover the Tailscale range, interface sorting (including
a Tailscale exit node), the listener expansion, the commands for private and
public pools (with `--chipnet` on Chipnet, which a rig needs to accept a
Chipnet `--address`), and the rendered page with and without Tailscale. Live:
the public GPU pool test in [farm.md](farm.md#tested) joined one rig at the
local-network address and one at the Tailscale address from `connect`.

## Connection info for devices, joining by one line, and the GPU pool from setup (2026-10-08)

#### PR #40

- **Connection info** (`i` on the server dashboard), modeled on ASICseer's:
  every address devices use, SV1 lines first (stock Antminer, Avalon and
  Whatsminer firmware speak only SV1) and SV2 lines with the authority key in
  the address, `stratum2+tcp://HOST:PORT/KEY` (the form ckpool and Braiins
  publish), each at this computer's local-network address and, when
  Tailscale is up, its Tailscale address (`src/reach.rs`; nothing is sent).
  The page says what the username means for a solo server, a public pool and
  a pool member, that the password is not checked, suggests Tailscale when it
  is not running, and, for ASICs elsewhere, a Pickaxe at their site joining
  this server over SV2. Number keys copy a line (OSC 52, plus `clip` or
  `pbcopy`). The header's "Point devices at" line names `i`, and the JSON
  start line lists the addresses under `connect`.
- **Joining by one line**: `--upstream` and the setup's Pool row accept the
  pool's `stratum2+tcp://HOST:PORT/KEY`; the key may then be left out, must
  match when given twice, and an SV1 pool address is refused with the reason.
- **Run a pool → GPU pool** starts the public GPU pool (as `mine --rigs-listen
  0.0.0.0:3340 --rigs-only --rigs-public`): the token's servers instead of a
  node, the fee as a share of each rig's mining time, and a `q` fee address,
  since token claims pay P2PKH. P2Pool v2 is now the only option marked
  coming soon.

Evidence: host tests cover the address lines for every listener kind, the
page for each mode, the JSON lines, the one-line pool address (with and
without a key, IPv6, SV1 refused) through to the pool list, and the setup's
GPU pool rows, validation and result.

## Node failover for the ASIC server (2026-10-08)

#### PR #40

The server keeps every configured BCH node, in failover order starting with
the first that gave a synchronized template at start. When its node gives no
template (down, refusing the login, or still synchronizing), it moves to the
next one at once; the failed node waits at the back, so a later failure moves
on again. Work on the old node's templates is revoked and devices take a new
job; template generations keep counting, so no job identifier repeats. Saved
blocks are whole blocks, so a block waiting for a node's reply goes to
whichever node is in use. The dashboard and the status show "node N of M"
and the number of moves.

The block journal is now bound to its network and payout script instead of
also the node it was first written with (PR #38's binding), which refused to
open when the server started on its second node. A journal written under the
old binding opens while its node is still configured and is rebound.

Evidence: host tests cover moving to the second node when the first stops
answering (a block found then goes to the second node only) and back when the
second fails, the rebinding of a journal written under the old binding (and
its refusal without that node or with another payout), and the dashboard
label. Not yet run against two live nodes.

## PHOTON from the miner's own node (2026-10-08)

#### PR #40

With a BCH node configured, the PHOTON job source is the node: the session
(`ElectrumSession` with a node link) finds the baton with `scantxoutset` on
the covenant script, follows it with `gettxout` and the mempool's successor,
rescans when it loses it, and answers the other calls the miner makes with
standard RPCs: `getmempoolinfo` and `getnetworkinfo` for the relay fee,
`sendrawtransaction` for claims, and `getrawtransaction` for a transaction by
id, which without a transaction index looks in the mempool, then in the block
holding one of its unspent outputs, then in the last 24 blocks. The runtime
connects to the node first, at start and on every reconnect, and to the
Fulcrum servers when no node is configured or none can give the PHOTON state;
on the node, each refresh is the node's state, with no Fulcrum proof. Before,
the node was a fallback route trusted only within a proof lease from Fulcrum,
so mining needed Fulcrum to start and stopped after a claim without it.

Evidence: host tests with a scripted node cover the RPC answers and the
transaction lookup without an index (and a pruned block's data counting as
not found). Live on 2026-10-09 against a pruned Chipnet Bitcoin Cash Node
29.1.0 with no transaction index (`real_node_gives_the_photon_job_alone`,
opt-in): the PHOTON job came from the node alone by its UTXO-set scan (0.4 s
on Chipnet), the baton's transaction was found by id, resuming from the
known baton took 18 ms, and the job matched a public Chipnet Fulcrum's
(baton, reward and target) at the same tip. That test found that BCHN's
`scantxoutset` reports no `height` or `bestblock` (Bitcoin Core's fields),
so the node route had never started against BCHN; the tip read right after
the scan now stands in, checked by every later read. A coordinator with no
GPU then mined from the node for three minutes beside a running GPU miner,
its job and refreshes from the node, with no reconnect. Not yet live: a claim
sent through the node.

## Worker names on the workers page (2026-10-09)

#### PR #40

A device is named by the worker name in its username: the part after the
payout address (`ADDRESS.rig1` gives `rig1`), or the whole username when it
is not an address, at most 24 printable characters, with its session number
to keep labels unique (`rig1 #3`). An address alone keeps the generated label,
so payout addresses are still never shown. SV1 devices are named by the
adapter when they authorize; native SV2 devices by their channel's user
identity (the adapter's own channel identity is not a name).

Evidence: host tests cover names from addresses with and without a prefix,
account-style usernames, bare addresses and control characters, and the label
change.

## Joining a GPU pool or farm from the setup (2026-10-09)

#### PR #40

GPU mining's settings have a Mining row: alone (claims to your address), or
join a GPU pool or farm. Joining asks for the coordinator's address and key
(the one-line `stratum2+tcp://HOST:PORT/KEY` fills both) and your payout
address, which a public pool claims your wins to; it shows no Fulcrum or node
rows, since a rig takes its jobs from its coordinator. Start mines as a rig
of that coordinator on the chosen GPUs, as `--coordinator` does (both use one
`run_as_rig` path), printing its status lines.

Evidence: a host test walks the setup from the Mining row to the result,
including the one-line address filling the key and the checks for a missing
coordinator address.

## A coordinator with no GPU takes no GPU lock (2026-10-09)

#### PR #40

A coordinator that mines with its rigs only (`--rigs-only`, or Run a pool,
GPU pool) no longer takes the GPU lock, which keeps two miners off the same
GPUs: it uses none, and the lock stopped a farm's coordinator from running on
a PC that also mines. Any process with GPUs still takes it.

Evidence: the live run above, beside a running GPU miner.

## A pool's name in its blocks (2026-10-09)

#### PR #40

`stratum-v2 serve --pool-tag TEXT` (the setup's Pool name row for an ASIC
pool) writes the name into the coinbase script of every block the server
builds, after the node's coinbase flags and before the extranonce, as
ckpool's and ASICseer's pool identifiers are; at most 20 printable
characters, refused with `--upstream`, where the pool builds the blocks.
Nothing is written by default.

Evidence: host tests cover the name in the coinbase with the coinbase parts
still fitting around the extranonce, a name too long for the 100-byte script
refused, the flag, and the setup row.

## Best share, recent blocks and the starting difficulty (2026-10-09)

#### PR #40

The server records each share's hash difficulty: the workers' best shares
(in the JSON status per device) and the server's best since start with its
worker; the overview (Tab) and the status list the last blocks found (height,
hash, worker, age, and the node's answer once it has one), as ASICseer's
dashboard does. `--start-difficulty N` sets the share difficulty devices
start at (default 4096, the compact target 0x1b0ffff0).

Evidence: host tests cover a found block and the best share recorded through
the server, the overview line, and the difficulty-to-target conversion
(difficulty 1 and 4096 match their compact targets).

## A watch view for a miner without a screen (2026-10-09)

#### PR #40

A `--no-tui` miner, such as a GPU farm's or public pool's coordinator running
as a service, saves its status line once a second beside its config
(`NAME.mine-status.json`, written whole and without the payout address).
`pickaxe watch` with the same `--config` shows it read-only: the coordinator's
farm and a row per rig (name, GPUs, rate, winners, minutes connected), or a
miner's own GPUs, with the source, height and winners, and says when the
miner stopped updating. It never connects to the miner, as `stratum-v2 watch`
for the ASIC server.

Evidence: a host test saves a coordinator's status (no payout kept), and the
view shows the farm, the rig row, a stale status and a GPU miner's rows. Live
on Chipnet on 2026-10-09: a coordinator with no GPU, run without a screen
beside a running GPU miner, saved its status (mining, the height, its rig
listener and job source) with no payout address in it.

## One row per device (2026-10-09)

#### PR #42

The workers table keeps one row per device on the local network instead of one
per connection, and the label keeps its number (`rig1 #12` stays `rig1 #12`,
where it used to become `rig1 #15`), so the label follows the device.

- A device that connects again from the same local address takes over its
  offline rows there and the number of the one that went offline last.
- A device that comes back before its old connection is seen closed (after a
  power cut, a reboot or a pulled cable, the server notices a dead connection
  only when a write to it fails) gets its old number when that connection
  closes. The new connection must be the only one on the address that opened
  after the old one's last accepted share and has not taken over a row.
- A connection that closes within a minute while another one on its address is
  online (a firmware's short extra connection) leaves no row, and gives back
  any offline row it took over.
- Any other closed connection stays as an offline row, also when another
  device on the same address is online: behind a Tailscale subnet router, a VPN
  gateway or CGNAT many devices share one address, and a real device must show
  offline rather than vanish.
- Devices seen from this computer or from a public address keep one row per
  connection, as before. The address is used only to match rows and query the
  device; it is never shown or written to the JSON status.

Known limits:

- Behind one shared address, a reconnecting device takes over every offline
  row there, so another device's offline row can disappear and a number (and
  a name, until the device gives its own) can move to a different device. The
  Device panel refuses control on such an address (see the next checkpoint).
- When two or more new connections open on an address before an old one there
  is seen closed (several devices behind a shared address reconnecting at
  once, or a device that opens a second connection that early), the old row
  stays offline and the new connections keep new numbers.

Evidence: host tests cover a reconnect replacing the device's offline row while
another device's stays; a short extra connection leaving no row; a device that
mined for two minutes behind a shared 100.64.0.0/10 address staying as an
offline row while a short probe there still vanishes; a probe there that opens
after the device went offline giving its row back; a device back before its
old connection closes ending with one row and its old number, and two new
connections leaving the old row offline; a reconnect over several offline rows
taking the number of the latest to close; and a reconnected device keeping its
number and worker name whether it is named before or after its address is
known.

## The Device panel for any row (2026-10-09)

#### PR #42

The workers page highlights one row (arrow keys, PgUp/PgDn, Home, End). The
highlight is kept by the row's label, so it stays on its device when rows
re-sort and when the device reconnects. Enter (or `c`) opens the Device panel
for that row, online or offline. `c` used to open the controls of the row at
the scroll position, and only while it was online.

- An offline row opens with the address its device last connected from, and
  shows how long it has been offline. The panel names the network class
  ("local network", "Tailscale/CGNAT", "IPv6 local"), never the address.
- Device replies and errors are shown with every address hidden, and cut to
  240 characters. asic-rs errors name the request's URL, which holds the
  device's address.
- The device is identified on a background thread, for at most ten seconds,
  so the screen stays live. Offline devices are not polled, so an offline
  row's device is identified afresh. Its address may since belong to another
  device. Actions are offered only if the device identifies as the make and
  model the row reported while online. When that cannot be compared, they are
  offered only within ten minutes of the row going offline.
- What the panel identified stays known through the poller's passes, which ask
  only connected devices. A confirmed action then goes the way the panel
  listed.
- Identifying a device, and sending an action through asic-rs, no longer
  panic on tokio's timer, which was made outside the runtime. The panel kept
  "Identifying the device" or "Sending" after such a panic. The action path
  had it since PR #40.
- Restart on every Avalon is Canaan's reboot (`ascset 0,reboot,0`) instead of
  asic-rs's cgminer `restart`, which restarts only the mining program. asic-rs
  offers no restart for Avalon Home Q at all.
- Control is refused for a worker with no local address, and for a shared
  address (a Tailscale subnet router, a VPN gateway or CGNAT). An address is
  shared when two connections from it are each a minute old, or once two
  devices there each kept mining for a minute after the other connected. That
  mark stays until no row from the address remains, so it holds after one of
  them goes offline or reconnects. A firmware's short extra connection does
  not count, nor does a device's dead connection, which stopped mining before
  the device's new one opened. Several connections from one address, all
  under a minute old, are refused for that minute.
- A reconnecting device keeps its worker name with its number until it gives
  its own, so its label never reads "Device …" in between.
- The address map beside the rows is gone: each row keeps its device's
  address, used only to ask or control the device. A private address list
  (label, address, refused) is ready for the owner-only file `stratum-v2
  watch` will read. Only the server's modules can read it, and the status
  JSON stays without addresses.

Known limits:

- Two devices of the same make and model cannot be told apart. An offline
  row's address that passed to an identical device is controlled.
- A second device behind a gateway is not refused during its first minute
  there, unless two devices were already seen mining side by side there.
- While a device's dead connection still looks open beside its new one, both
  over a minute old, control is refused until the dead one is seen closed.

Not yet: fan, power and pool settings, device logins, and the panel in
`stratum-v2 watch`.

Evidence: host tests cover Enter, `c` and Ctrl+C on the workers page (Ctrl+C
does not open the panel); the panel opening for the highlighted row rather
than the top one; the highlight following its device when it goes offline,
when it reconnects before naming itself, and when the rows re-sort; an offline
row opening with its last address and its offline time, never the address; a
worker with no address, a shared one or an unsettled one offering nothing; an
offline row offering actions for the same make and model at any age, never for
another, and within ten minutes only when they cannot be compared; device
errors shown without IPv4 or IPv6 addresses (with or without brackets and
ports) and without reqwest's "for url" part; a choice needing a confirmation,
with the reply of a sent action shown; the panel's device staying identified
through a poller pass while another device is forgotten, and the time limit
around an asic-rs action made inside the runtime; offline rows keeping their
address while the poller skips them; two long-lived workers on one address
refused while a 5-second probe is not counted, and two young ones not settled
yet; two devices that mined side by side refused after one goes offline,
after it reconnects and after their rows are merged away, until no row
remains, while a single device's dead connection leaves no mark; the private
address list never reaching the JSON or Debug output; and Restart on Avalon
Nano 3s, A-series and Home Q never going through asic-rs, with Pickaxe's own
Restart sending exactly `{"command":"ascset","parameter":"0,reboot,0"}` to a
loopback stand-in. No device was contacted.

## Profiles keep their mode (2026-10-09)

#### PR #42

A saved profile now remembers what it starts: plain GPU mining, a rig joining
a GPU pool or farm, ASIC solo, ASICs at a pool, an ASIC pool or a GPU pool,
with that mode's pool values (the pool or coordinator to join and its key, a
pool's fee, where an ASIC pool's fee comes from, the fee address and the
pool's name). Opening the profile puts the setup back in that mode, so the
pool rows and values are there again; before, every profile reopened as GPU
mining. The profile list names each mode ("ASIC pool · Chipnet · BCH · fee
1.50%") and never shows a pool's address or key, or any address. Plain GPU
profiles save exactly the bytes they did before.

Known limit: a profiles file holding a saved mode is refused by older Pickaxe
versions, which do not know the new field.

Evidence: host tests save and reopen an ASIC pool, a rig and an ASIC joining a
pool, check that one profile's values never carry over to the next or to a new
profile, that the list hides pool addresses, keys and names, that a plain GPU
profile saves no `server` field, and that the saved values are refused when
the setup would refuse them (a name over 20 characters, joining without an
address, a space in the address, a fee on solo mining, a bad fee address, an
ASIC pool's name or fee source on a GPU pool).

## Fan and power on the Device panel (2026-10-09)

#### PR #42

The Device panel now sets fans and power where the device allows it. "Fan
speed…" takes `a` for automatic or a speed in percent; "Power mode…" offers
Low, Normal and High, and "Power limit…" takes watts. Every value is checked
against what the device accepts and then needs a confirmation, as every other
action does.

Which device gets what:
- through asic-rs: fans on stock Antminer, ePIC and Proto firmware; a power
  limit in watts where asic-rs sets one (Braiins, VNish, WhatsMiner, Auradine,
  Proto, SealMiner, and ePIC through its tuning); named modes on stock Antminer
  and WhatsMiner when they offer no limit in watts.
- through Pickaxe's own commands: an Avalon's fan (`ascset 0,fan-spd`, -1 for
  automatic, otherwise 15% to 100%) and work modes (`ascset 0,workmode,set`),
  each offered when the device's `ascset 0,help` lists it or lists nothing
  readable; a Bitaxe's or NerdAxe's fan through AxeOS (`PATCH /api/system`,
  under `manualFanSpeed` from AxeOS v2.12 and `fanspeed` before).
- never: an Avalon's power through asic-rs, which sends watts text as a work
  level.

Not yet: Bitaxe's identify light, and VNish's power percentage.

Unverified on a device: the Avalon Nano 3's `help` listing and `fan-spd` range,
and whether it has work modes. The panel only reads `help` (read-only); no fan
or power setting was sent to a real device.

Evidence: host tests check the exact Canaan requests for an automatic fan, 60%
and the High work mode; a fan under 15% refused before anything is sent; a
device's refusal shown as it is; an Avalon's help listing read; a Bitaxe's fan
sent under `manualFanSpeed` or `fanspeed` as its firmware reports, automatic
sent without reading first, and AxeOS's 401 explained; which settings each
firmware is offered (stock Antminer, WhatsMiner, Braiins, ePIC, LuxOS, Proto,
Avalon with and without a help listing, Bitaxe); an Avalon's fan and power
never going through asic-rs; and the panel's fan and power pages refusing a
value the device does not take, asking for a confirmation, and sending nothing
until `y`.

## Device logins on the Device panel (2026-10-09)

#### PR #42

A device whose owner changed its login can now be controlled. When a device
refuses an action (HTTP 401 or 403, "unauthorized", Canaan's "username err",
WhatsMiner's failed decryption and similar), the panel keeps the action and
says to press `l`. The login page asks for what the firmware uses: a username,
filled in with its default (`root`, `admin` for Auradine, `seal` for
SealMiner), and a password, or a password alone (VNish, ePIC, WhatsMiner). The
password shows as dots and never appears in `Debug` output.

The login is used at once: asic-rs identifies the device again with it (stock
Antminer, Elphapex, VolcMiner and SealMiner log in even to be identified), and
the held-back action is sent again. It is saved only once the device accepts
it: when that action goes through, when the device identifies as one of the
firmwares above, or after the next action that goes through. A login the
device refuses is dropped and nothing is saved.

Saved logins live in `<config>.sv2-logins.json`, readable by the owner alone
(0600 on Unix; the owner's account alone on Windows), written whole through a
temporary file. Each is used only for its device's address and for the
firmware it was saved for; addresses outside the local network are never
saved or loaded, and a symlink or anything but a plain file is refused. The
server loads them at start; a file that cannot be read leaves devices on their
default logins and mining unaffected.

Not yet: the logins in `stratum-v2 watch` (it gets the panel in a later
slice), and Avalon's web login, which only its pool settings need.

Unverified on a device: no login was sent to a real device.

Evidence: host tests save and reload a login, use it only for its own
firmware, refuse public addresses, keep another Pickaxe's saved login when
saving, forget one, keep the file at 0600 on Unix, skip public addresses in a
file and refuse a directory or symlink; the atomic writer replaces a file whole
and leaves no temporary file; the firmware names that log in to be identified
match asic-rs's registry; a login is kept for its own firmware and forgetting
it forgets what was identified at the address; refusal texts are recognised
and unrelated errors are not; and the login page is offered only to firmwares
with a login, hides the password, and saves nothing when the device does not
take the login.

## Pools on the Device panel (2026-10-09)

#### PR #42

"Pools…" on the Device panel reads a device's pools on request (never while
polling) and changes them after a confirmation. The page lists the pools in the
device's order, marks this server (by host and port, from the server's own
connection addresses) and the pool it mines on now, and shows workers by name,
never a payout address. `n` puts a typed pool first; `h` puts this server first
with the worker the device already used here. The other pools stay as backups,
each once, as many as the device holds; the confirmation lists what the device
will hold, what is dropped, any firmware notes (VNish keeps host and port only,
LuxOS replaces its groups, passwords the firmware does not report become `x`,
restarts), and says that while pool 1 works the device's shares, blocks,
merge-mined token wins and donation go to pool 1.

How pools are read and written:
- through asic-rs (`get_pools`, `get_pools_config` where the firmware keeps
  passwords, `set_pools_config` with one group);
- Avalons: CGMiner `pools`, then Canaan's `setpool` slot by slot with the web
  login, then `ascset 0,reboot,0`. A field with a comma is refused before
  anything is sent, and the success message, which repeats the worker and its
  password, is never shown;
- Bitaxe: AxeOS `system/info`, then `PATCH /api/system` with the pool and its
  fallback, then `POST /api/system/restart`.

A pool address must carry its port; asic-rs would read a missing one as 80. An
Avalon without a known web login answers that it needs it, which offers `l`;
the pools are sent again after the login.

Unverified on a device: no pool was read from or written to a real device (the
operator's Avalon keeps its pools as they are).

Evidence: host tests check the pool plan (new first, each old pool once,
dropped listed, two slots on a Bitaxe), the port and scheme rules, an Avalon's
`pools` read in priority order, `setpool` sent byte for byte with the web
login then the reboot, the success message never passed on, a refusal shown
without the web password and recognised as a refused login, a comma refusing
everything before a request; a Bitaxe's pools read, written with the old
primary as fallback, then restarted; and the panel's pool pages marking this
server, hiding a payout address, needing a port, listing the change with its
consequences, offering this server with its worker, and sending nothing before
`y`.

## The Device panel in `stratum-v2 watch` (2026-10-09)

#### PR #42

`stratum-v2 watch` now highlights a row and opens the same Device panel as the
server's workers page, so a server running as a service (the operator's
Debian server) can have its devices controlled over SSH. The watch view acts
on the devices itself and never talks to the server. The server writes
`<config>.sv2-devices.json` once a second when it changes, readable by its
user alone and written whole: where devices reach this server, and each
worker's label with its local address and whether that address is shared.
The watch view reads it when a panel opens, checks the address again
(local network or Tailscale only, never a shared one) and uses the server's
saved device logins. Without the file (watch not run as the server's user) the
panel explains how to run it. The status file, which everyone can read and
which `--json` prints to service logs, still holds no device address.

Unverified on a device: watch was not run against the operator's server yet.

Evidence: host tests write and read the devices file, take a worker's address
from it, refuse a shared or unknown one, refuse an edited public address, read
the server's own addresses back, and show the "run watch as the server's
user" explanation when the file cannot be read.

## The Advanced section in setup (2026-10-09)

#### PR #42

The setup's settings page now has an **Advanced** section for the ASIC modes,
closed by default and opened with Enter (or Left/Right) on its header. It holds
what was missing from the setup or hard to find:
- ASIC solo: the donation, the start difficulty, the SV1 and SV2 ports, and
  the Fulcrum list.
- Join a pool: backup pools (one-line SV2 addresses with their keys, at most
  8, used in order), the username at the pool (the payout address by
  default), the donation and the SV1 port.
- An ASIC pool: where the fee comes from, the fee address and the pool's name
  (moved here), the start difficulty, both ports and the donation.

The donation row reads "1.50% of BCH and merge-mined tokens (1.00% of rewards ·
0.50% of work)" and goes from 0% to 100% in 0.5% steps. The start difficulty
halves or doubles with Left/Right or takes a typed number (1 to 2^48; 4096 is
the server's default). Ports are checked (1 to 65535, the two different).
When Start finds a wrong value inside the section, it opens the section at
that row. A saved profile keeps every value, and the server starts with them
(before, setup always used ports 3333 and 3336, the default start difficulty,
one pool and the payout address as the pool username).

Not yet: the GPU modes' rows in the section, and the server's own Advanced
page showing these start values.

Evidence: host tests check that the section hides its rows until opened, the
donation row's text and its 0%, the start difficulty's steps and typed value,
the port checks, backup pools needing their key, a profile keeping and
restoring every value, saved values refused as the setup refuses them, and
Start opening the section at a wrong fee address.

## The server's Advanced page shows its start values (2026-10-09)

#### PR #42

The running server's Advanced page (`a`) now lists what it started with,
read-only, with a note that they are changed in the setup's Advanced section
or on the command line and apply after a restart: its listening addresses,
the start difficulty and the vardiff rule (20 shares a minute per device), the
pool's name, a public pool's fee and where it comes from (to "your payout
address" or "another address", never the address itself), and for Join a pool
the pools in failover order and whether the pool knows it by the payout
address or another username.

A donation changed on that page is now also saved into the setup profile the
server started from. Before, starting from the profile again restored the
profile's own donation and the change was lost. The save fails closed if the
profile was renamed or removed meanwhile.

Evidence: host tests render the page with a public pool's start values (both
ports, 65,536, the pool's name, the fee "to another address") and a joined
server's pools and username, and save a changed donation to the config and to
its profile, refusing a profile that is gone.

## Merge-mining core: commitment, tree, proofs (2026-10-09)

#### PR #42

The foundation for merge-mining BCH covenant tokens on the same SHA-256
work (`src/stratum_v2/merge/`, v1 draft): no token exists yet, both
networks' registries are empty, and nothing runs it, so every coinbase stays
byte for byte as before.

- One commitment per coinbase, in output 0: value 0, `OP_RETURN` "CTMM",
  version 1, the aux tree root, its height (at most 16) and nonce, 53 bytes.
  A covenant finds it from the coinbase's single input, without a search.
- A tree of 176-byte leaves, one per token and mode: Case A tokens win on a
  share that meets their target (no BCH block needed); Case B tokens need a
  found block and claim through a zero-value keyless ticket output after
  the payouts, spendable once the block is mature. Each leaf binds its
  anchor, the job's payout and target, and the donation's split.
- The donation covers merge-mined tokens through the one BCH ASIC setting
  (0% to 100%, default 1.5%): two thirds as a split in the claim, bound in
  miner jobs' leaves, and one third through the same work rotation as BCH
  (donation-work jobs bind the Pickaxe donation address). A public pool's
  fee reaches tokens only through its fee-work jobs, not its coinbase share.
- `AuxProof` v1 (canonical bytes) and a Rust reference verifier that checks
  a proof step for step as a covenant would, for both cases.
- The block journal accepts a solved block with the commitment and tickets
  (zero-value, exact scripts only) and still refuses any other extra output.

Evidence: host tests cover the commitment's golden bytes, the leaf layout, slots,
layouts and branches, the proof encoding, a Case A and a Case B proof that
verify and one failing check per mutation (coinbase, header, branches,
slot, category, payout, split, stale anchor, moved output 0, script length,
output count, target encoding, ticket and start height), the unchanged
151-byte coinbase, golden 204- and 250-byte coinbases with a token, and
journals holding token blocks beside current and pre-donation ones.

## Merge-mining share path: token wins and the share-target floor (2026-10-09)

#### PR #42

Every accepted share is now checked for merge-mined token wins
(`src/stratum_v2/channel.rs`, `wire.rs`). No token is registered on either
network, so nothing changes at runtime yet. Jobs carry no merge-mining, and
coinbases stay byte for byte as before. The server takes no wins until its
wiring lands.

- A job built from a template with tokens gets its own merge-mining. Its
  leaves bind the job's payout and the donation's split. The payout is the
  miner's, the Pickaxe donation address in donation-work jobs, or a public
  pool operator's in fee-work jobs. The job's coinbases carry the commitment
  and the tickets. This holds on standard and extended channels, and for SV1
  firmware through the adapter.
- Each share costs one compare with the job's easiest Case A target. A
  winning share hands on its job, header, coinbase and merkle branch: all a
  proof needs. It goes beside the acknowledgement, whether or not the share
  is also a block. Case B tokens win only with a found block.
- A duplicate never yields a second proof. A share over the per-job cap is
  refused, so it yields no proof.
- While Case A tokens are merge-mined, a device's share target is made
  easier, up to the easiest token target. It is never more than 15 times
  easier than vardiff's target (about 5 shares a second), and never easier
  than the device allows. Within that range firmware sends every hash that
  wins a token. A token target easier than that is mined best effort: only
  hashes that meet the capped target reach the server.
- Vardiff counts and estimates only at its own target.
- The best-share record uses the hash the check computed. That saves one
  double SHA-256 per share.

Evidence: host tests cover a token win's header, coinbase and branch on
standard and extended channels. Its proof verifies as a covenant would check
it. Other tests cover shares without tokens, a block's Case B win and
duplicates. They show that public-pool, donation-work and fee-work jobs bind
their own payout and split, and that retained jobs keep their own commitments.
Tests cover the floor and its cap, and vardiff ignoring shares that only the
floor accepts. Session tests cover token wins in the responses beside a block,
the best share from the carried hash, and new channels and jobs carrying the
floor.

## Merge-mining on the server: claim worker, proofs and the test token (2026-10-09)

#### PR #42

The server now merge-mines whatever token set it is given, and proves its
wins. No token is registered on either network, so by default nothing changes:
no hub, no commitment, the same coinbases.

- Jobs carry the current token set. When a token's state changes, the server
  publishes the same template again with the new set, without asking the
  node, so devices get new jobs on the same parent and a failing node call
  cannot revoke their work.
- A share's token wins go from the device thread to a claim worker through a
  bounded queue (64) with `try_send`: the acknowledgement never waits, and a
  full queue drops the win and counts it. Each token state is handed on once.
- The claim worker builds each won entry's proof and checks it with the
  reference verifier exactly as a covenant would, for the job's beneficiary
  and the donation's split. A proof that fails its own check is never kept and
  turns token claims off. Passing proofs are saved to
  `<config>.sv2-token-proofs.json`, owner-only and written whole, the newest
  256 kept, with status "proven" (no token has a covenant or claim builder
  yet, so nothing is broadcast). A journal that cannot be written turns token
  claims off and leaves BCH mining as it is.
- The dashboard's records line shows token wins (token, case, worker, any
  dropped) or why tokens are off, and the status file lists them without any
  address. Join a pool's connection info says merge-mined tokens are off at a
  pool, since the pool builds the blocks. `stratum-v2 status` names merge
  mining as a v1 draft.
- The hidden `stratum-v2 serve --merge-test-token <DIFFICULTY>` merge-mines
  the Chipnet test token in both cases, with a simulated baton that moves to
  each winning header. Its registry row is on Chipnet, so mainnet refuses it.

Not yet: Case B ticket maturity tracking (a found block's ticket is proven at
once and the 100-block wait is left to a token's claim builder), and the
opt-in live check that BCHN accepts a template with the commitment and a
ticket (`validateblocktemplate`).

Evidence: an end-to-end host test mines the test token through the real
server on standard and extended channels: one share wins Case A and, being a
block, Case B; both proofs are proven, saved owner-only without the payout
address, listed on the dashboard, and the baton moves (a new set serial); the
mock node accepts the block with the commitment and the ticket. Other tests
cover the test token's difficulty and Chipnet-only rule, the records line and
status file, and the capability report.

## Join a pool follows the pool's group channel (2026-10-09)

#### PR #42

A bug fix for Join a pool at SRI-based SV2 pools (such as LoneStrike's BCH
stack). Those pools put every extended channel in its connection's group
channel and send every job refresh, new parent and target to the group. The
SV1 adapter accepted only its own channel ids, so a device there got its first
job and then dropped with "unexpected firmware job".

The adapter now records each channel's group (from the pool's channel reply)
and takes jobs, parents, targets and closes addressed to it on every lane in
the group: the device's own and the donation's. `SetGroupChannel` moves its
lanes between groups. Share verdicts stay per channel. Closing the group
closes the device's lane, so the device reconnects. The adapter now names its
version in its `SetupConnection`, so a Pickaxe pool can tell adapters that
follow group channels.

Evidence: host tests check that group jobs and parents reach the device as
notifies (clean for a new parent, not clean for a same-parent refresh), that
one group job feeds both lanes so the device switches to donation work by job
alone, that `SetGroupChannel` moves a lane while other ids are still refused,
and that closing the group closes the device's lane. Not yet run against a
live SRI pool.

## Chipnet claims take the mainnet path (2026-10-09)

#### PR #42

Chipnet and mainnet are now one claim path. Two Chipnet-only shortcuts in the
GPU miner's claim code, left from the retired Chipnet batch-payout preview,
are gone: a Chipnet claim is checked with the saved node's
`testmempoolaccept` and broadcast by the same source preference (Fulcrum or
the node, falling back to the other) as a mainnet one. The curated node list
is the network's own data (`MiningNetwork::node_rpc_bootstrap`; empty on both
networks), as Fulcrum's is.

Evidence: a host test runs the gate on both networks against a loopback node
that rejects the claim: skipped without a saved node, and stopping the claim
with the node's reason on both; the existing Chipnet preflight test still
passes. No Chipnet comparison remains in the runtime outside tests.

## Template core: the coinbase path once per template (2026-10-09)

#### PR #42

No behavior change; groundwork for Template Distribution and Job
Declaration. A template now computes the coinbase's merkle branch once, when
it arrives, and every coinbase's merkle root is its hash folded up that
branch: log2(n) hashes instead of the whole tree. An extended channel's share
rebuilds its coinbase from the job's parts (the prefix ending in the job id,
the channel's extranonce prefix, the device's extranonce and the suffix with
any merge-mining outputs), which are exactly the bytes the device hashed,
instead of recomputing the payouts (a CashAddr decode) and the tree for every
share.

Evidence: host tests check that the folded branch gives the whole tree's root
and has the right depth for 0 to 17 transactions, and that a coinbase rebuilt
from parts equals the full build byte for byte, with the same root, for 0 to
17 transactions and for miner, donation-work and fee-work payouts. All
existing share, token and server tests pass through the new path.

## Template Distribution server (2026-10-09)

#### PR #42

`serve --tp-listen ADDRESS` (the setup's **Serve templates** row, on port 8442
on mainnet and 48442 on Chipnet) serves this node's templates over SV2
Template Distribution with the mining listener's key, to at most 8 clients:
SRI's pool, a Job Declaration client, P2Pool or another Pickaxe. A new parent
comes as a future `NewTemplate` and its `SetNewPrevHash`, a same-parent
refresh as a current template only; the coinbase prefix is the height push
alone and no coinbase outputs are required. Transaction data comes in block
order, or as `stale-template-id`, `template-id-not-found` or
`template-too-large` beyond SV2's single 16 MiB frame. A template the client's
reserve does not fit is withheld. Solutions are assembled into blocks with
BIP141's one 32-byte witness item stripped, checked (prefix, version, proof of
work, size), saved in the owner-only relay journal and submitted with the
block journal's retries; a solution that fails Pickaxe's checks still reaches
the node once per 10 seconds. Device sessions keep their 1 MiB frame limits;
template sessions take 64 KiB in and send up to 16 MiB.

Evidence: host tests with an SRI-shaped client on the reference crates over
Noise against the real server: golden bytes for `NewTemplate` (45 bytes) and
`SetNewPrevHash` (80 bytes); setup refusals; no template before the
constraints and a silent client closed; future and current templates and
rising ids; an SRI-shaped coinbase with a BIP141 witness stripped, relayed
once and finished in the relay journal while the block journal stays empty;
two locally refused solutions giving one submission; transaction data for
current, stale and unknown templates and at the 16 MiB and 65,535 limits; a
withheld template that follows smaller constraints; a relayed block surviving
lost replies and a restart without the listener; the 8-client cap; a 70 KB
client frame closing the session; device frames still capped at 1 MiB. Not
yet run against a live SRI pool or P2Pool.

## Job Declaration server, Coinbase-only (2026-10-09)

#### PR #42

A public pool started with `--accept-job-declaration coinbase` accepts miners'
own templates. A client's Job Declaration session shares the pool's SV2 port
and key; it is set up for Coinbase-only (Full-Template is refused with
`unsupported-feature-flags` for now) and allocates PX v1 tokens, each bound to
the miner's payout address, alive 10 minutes and good once, at most 20 a
minute per connection. The client's mining connection negotiates work
selection; its extended channels are custom-only with 16 rollable bytes, and
`SetCustomMiningJob` is checked against the pool's template (parent, bits,
version, start time, BIP34 prefix, coinbase version and size) and the token's
payout rule: the whole donation and the whole fee in the coinbase, the miner
funded, at most one exact commitment at output 0 and no CashTokens outputs.
Custom job ids carry 0x8000_0000. Shares rebuild the client's coinbase; a
block on a custom job is listed as submitted by the miner's node and never
journaled. A JD session leaves the workers table.

Evidence: host tests for the output codec, the PX v1 layout (golden bytes) and
token book (identity, secret, lifetime, single use, eviction), the payout rule
at 1.5% and 1% (exact amounts pass, one satoshi less fails; per-script sums; a
100% donation), the commitment shape, the JD session (setup, allocation, rate,
unknown users), every SetCustomMiningJob error code, custom-only channels and
custom-job shares and blocks; and a loopback test where a scripted client
allocates a token, sets a custom job paying 304,734,375 / 3,078,125 /
4,687,500 and mines it through the real server. Not yet run against another
implementation's client.

## Job Declaration client, Coinbase-only (2026-10-09)

#### PR #42

`serve --upstream stratum2+tcp://POOL:3336/KEY --job-declaration coinbase`
runs the miner's node and a local server, as solo mining does, and an uplink
to the first pool: a Job Declaration session and one work-selection channel,
both pinned to the pool's key, with two tokens in hand. While the uplink holds
a plan (the pool channel's prefix, the pool's outputs and rates from its PX v1
token, its target), the local server publishes jobs that pay the pool's
outputs and nest their extranonce inside the pool channel's (job id, pad,
lane, device), and hands each template to the uplink, which declares it with
SetCustomMiningJob. Accepted local shares meeting the pool's target are
forwarded with try_send, held (256 per job) until the pool confirms the job.
Blocks go to the JD journal and the miner's node. When the pool refuses a job
other than for a tip race, or the link fails, the plan goes, the local server
offers no work and the SV1 adapter moves devices to the pools' own jobs; the
uplink retries after 30 seconds. Non-Pickaxe pools (opaque tokens) are refused.

Evidence: host tests for the plan's outputs against the pool's rule, the
nested layout and forwarded bytes, the JD journal, and two loopback tests: a
Pickaxe client mining at a Pickaxe pool (an SV2 device's block pays 304,734,375
/ 3,078,125 / 4,687,500, reaches the miner's node only, the pool accepts the
forwarded share on the custom job and lists the block as submitted by the
miner's node), and a client falling back with the pool's reason at a pool
without Job Declaration. Not yet run in two processes on Chipnet.

## Template Distribution client (2026-10-10)

#### PR #42

`serve --template-provider sv2tp://HOST:PORT/KEY` (repeatable) takes
templates from SV2 Template Providers before the configured nodes. The
server's template thread now works with any `TemplateSource` (a node over
JSON-RPC or a provider) and fails over across them in order, with
server-owned generations so job ids never repeat across sources. The client
pins the provider's key (unpinned only on loopback), declares a reserve of 122
bytes (plus 53 and 46 per Case B ticket with tokens), follows NewTemplate and
SetNewPrevHash, fetches transaction data (2 seconds at most), builds the
template with the same checks as a node's, and confirms each new parent on
the selected network through the first node or the network's Fulcrum
servers. Solutions go back as SubmitSolution and count as accepted once the
provider names the block as a parent; the first node also gets the whole
block.

Evidence: host tests for provided templates (height, flags, size limit, and
each refusal), provider addresses, the reserve (122, 221 with the test
token), a provider breaking the protocol, the network guard refusing and
then confirming, and two loopback runs: a Pickaxe mining on another Pickaxe's
template server (key pinned), whose block reaches both the provider's node
(relayed) and its own (whole-block fallback), and the same with an unpinned
provider on this computer. Not yet run against a node bridge or Knuth.

## Job Declaration, Full-Template (2026-10-10)

#### PR #42

Both sides gain Full-Template. A public pool started with
`--accept-job-declaration` (alone: both modes; or `full`, `coinbase`) checks
each `DeclareMiningJob`: the token, the coinbase's shape and height, the
payout rule, canonical transaction order and the version; it asks for the
transactions its node lacks (from its current and two previous templates and
the latest 128 MiB clients provided, at most 2,048 per round and 16 rounds),
and its first node checks the whole block with BCHN's
`validateblocktemplate` on a connection's first declaration per parent, then
at most once a minute, and always when the client provided a transaction (4
checks at once at most). Refusals that are races at a new block are
`stale-chain-tip` with the node's reason in the details. The declared token
sets exactly the declared job; its blocks, from a share on the custom job or
the client's `PushSolution` (matched by proof of work), are saved in the
pool's JD journal and sent to the pool's node. Full-Template sessions take
16 MiB frames; Coinbase-only ones keep a device's limit.

The client (`--job-declaration`, Full-Template unless `coinbase`) declares
each template with its transactions, sends those the pool asks for, sets the
custom job with the declared token, and pushes its blocks to the pool; a race
drops one template, and the fourth other refusal in a row on a parent falls
back to the pool's own jobs. Templates now share their transactions' bytes,
so a pool's declared jobs and a server's template copies do not copy them.

Evidence: host tests for the declared coinbase (parse, each refusal, and the
declared path equal to the template's parts for 0 to 17 transactions), the
token book (declared tokens, `job-not-yet-validated`), the server session
(setup per mode, one missing-transaction round then a declared token, the
validator's cadence, the tip-race mapping, each structural refusal,
`missing-txs` after 30 seconds, PushSolution found by proof of work),
`SetCustomMiningJob` on a declared token (each mismatch, and a whole block
from a share), the client (65,535 transactions, one-frame limit, four
refusals), and a loopback run: the pool's node has 2 of the client's 3
transactions, one round fetches the third, `validateblocktemplate` runs once,
and a CPU device's block reaches both nodes with the same hash. Not yet run
against BCHN on Chipnet, nor with SRI's client.

## Fallback pools and coming back (2026-10-10)

#### PR #42

Solo mining takes `--fallback-pool stratum2+tcp://HOST:PORT/KEY` (repeatable,
with `--sv1-listen`, not with `--upstream` or `--public`): while the node
gives no work, the server stops offering it, device sessions end after the
3-second grace, and the SV1 adapter opens their next sessions at the first
fallback pool that takes them, with the donation as at any pool. The server
marks its preferred source while it publishes work (the node, or under Job
Declaration its plan); a session at a fallback pool ends once that source has
served for 30 seconds without a break, so devices come back to the node or
to Job Declaration by themselves.

The Job Declaration client tries the pools in `--upstream` order after a
failure, waiting 30, 60, 120, then 300 seconds as failures follow each other
(a session with accepted custom jobs starts again), and falls back when the
pool rejects 5 of the last 20 shares sent (races at a new block not counted).
Under Job Declaration a newer template on the same parent and plan reaches
devices only once the pool accepted its custom job, and templates on one
parent go to the pool at most once every 5 seconds.

Evidence: host tests for the return rule (30 seconds without a break), the
retry waits, the rejection window, same-parent gating, the `--fallback-pool`
flags, and a loopback run: an SV1 device on the node's server moves to a
fallback pool when the node stops answering and back once it answers (the
return time shortened to 100 ms). Not yet run live with a stopped Chipnet node.

## Group channels in the mining server (2026-10-10)

#### PR #42

The server groups the extended channels of one payout identity on a
connection and sends each new template as one job frame to a group of two or
more; first jobs, vardiff jobs, targets and share answers stay per channel.
A grouped connection takes up to 256 channels. No groups form for standard
jobs, custom-only channels or a Pickaxe adapter from before group support.

Evidence: host tests for one frame per refresh, each member rebuilding the
same block header from the group's job and its own prefix, standard and
single-member connections keeping their own frames, public-pool groups by
payout, per-channel vardiff, group close and refused group ids, the 256 and
32 caps, the legacy adapter, and a loopback run: a scripted proxy with two
channels mines a block on its own first job and another on the group's job
after the parent changes. Not yet run with SRI's translator.

## SAFA reference core (2026-10-10)

#### PR #42

No runtime change. `merge/safa.rs` holds the SAFA draft as data (the version
rule, the target byte order, the adjustment, the fee allowance, the emission
divisor, the age limit, the time rule), the 80-byte commitment, the
adjustment in the covenant's integer order, the compact target encoding, and
the claim checks with two guards: a win's hash must be positive and at or
below its target (the draft's signed compare also passes negative hashes),
and a target that would need a 33-byte encoding is refused (it would freeze
the thread). `merge/header.rs` holds the keyless forwarder that is both a
SAFA payout script and the coinbase devices hash (73 bytes on extended
channels and SV1, 65 on standard ones, with a `PXH1` tag that keeps SRI's
BIP141 check from misfiring), and the `HeaderWin` v1 record.

Evidence: host tests for the canonical template's vectors (the release
scenario passes only through the sign bug), the adjustment table, the
compact encoding against Bitcoin's for sizes 3 to 32, the freeze guard, each
claim mutation, the version rules, the forwarder's golden hashes, SRI's own
BIP141 check on every prefix, and the win record. No token is deployed.

## Token work in the share path (2026-10-10)

#### PR #42

No change while BCH is selected. A template's work is a BCH block or a
token's job (`Work::Token`); a token job has no transactions, a SAFA job's
coinbase is the forwarder (the standard root is its HASH256; extended parts
are its prefix and suffix with an empty path), and token work is never a
block. A SAFA win is a share whose hash is positive and at or below the
token's target. The version rule comes from the template: BIP320 for blocks
and Case A, the token's own for a header-shaped token; for a token that
fixes its version the server refuses clients that require rolling and sets
the fixed-version bit, and the SV1 adapter asks for no rolling, answers
`mining.configure` without it and takes jobs that disallow it.

Evidence: host tests for block suppression and the positive-hash win, the
version rules, the script coinbase and its parts, the fixed-version setup and
jobs, the adapter's fixed version, and a loopback run where an SV2 standard
device, an SV2 extended device and SV1 firmware through the adapter mine a
SAFA job: every share accepted, none journaled, nothing at the node.

## Token worker and the Chipnet ASIC test token (2026-10-10)

#### PR #42

A server can mine an ASIC-exclusive token instead of BCH with no node: its
only template source is the token worker (`merge::source`), which builds
each job from the token's thread. `--asic-token NAME` is refused while the
header-token registry is empty (it is on both networks); the hidden
Chipnet-only `--asic-test-token DIFFICULTY` runs the ASIC test token (SAFA's
canonical layout with BIP320 rolling, a 1.5% donation minimum, no covenant)
on a simulated thread. Device threads hand wins to the worker through a
bounded queue; the worker checks each as the covenant would, saves it
(owner-only, mode H), and moves the thread to the winning header, which
re-issues jobs. The token's donation is a share of the work, at least its
minimum. The overview and the status file show the token's wins.

Evidence: host tests for the registry rows, the token donation rotation,
the worker (a proven win moves the thread and is saved once; a second win
on the old thread is stale; a win paying another script turns proofs off; a
full queue drops and counts), the flags' conflicts, the status file, and a
loopback run where a device's share on the test token is proven and its
next job follows the win, with nothing at the node. Not yet run with
hardware.

## Setup rows for Job Declaration and fallback pools (2026-10-10)

#### PR #42

The setup's Advanced section now sets what only flags could: Join a pool's
**Your templates** (Job Declaration off, full template or coinbase only; the
BCH node row follows it while it is on, and Start asks for the node, a valid
payout address and no other username at the pool), ASIC solo's **Fallback
pools** (up to 8, each with its key, in order), and an ASIC pool's **Miner
templates** (off, both modes, full template only or coinbase only). Profiles
keep them (each refused outside its mode), and the server starts with
`--job-declaration`, `--fallback-pool` or `--accept-job-declaration`. The
setup's server command is now built in one tested place
(`tui::asic_serve_command`) instead of in `main.rs`. The server's Advanced
page names the Job Declaration mode when it declares; Connection info says
that a declaring server's node builds the blocks (and that merge-mined
tokens are not added under Job Declaration yet), and a pool accepting it
tells its miners where their templates go; `stratum-v2 watch` shows Job
Declaration's state and counts, as the client and as a pool.

Evidence: host tests drive each row through its values, check the Node row
appearing under Your templates, Start's checks, fallback pools refusing a
pool without its key or a ninth pool, profiles keeping and restoring each
value, saved values refused outside their mode, the serve command each
setup makes, the Connection info notes, and the watch header's Job
Declaration lines read back from a saved status. Not yet run from the setup
against a live pool.

## ASIC-exclusive tokens in the setup (2026-10-10)

#### PR #42

The setup's ASIC targets list now offers an ASIC-exclusive token for real:
it names the network's registered tokens, or says none is deployed on the
network (none is yet). With one chosen, solo mining mines it instead of BCH
with no node: a Token row, the payout address, and under Advanced the
token's own donation (never below its minimum, 0.5% steps), the start
difficulty and both ports; a fixed-version token carries a warning. Start
needs a registered token and a valid payout address, and starts `serve
--asic-token NAME` without templates or fallback pools. Profiles keep the
token and its donation, measured against the token's own minimum. The server
names what devices mine: the overview's title, the token's difficulty on
its line, `"mode": "asic-token"` in the status file, the token's donation on
the Advanced page, and a note on Connection info.

Evidence: host tests for the empty list's texts and refusal, choosing among
tokens and the fixed-version warning (with a test list), the donation row's
minimum and steps and what the profile saves, Start needing no node, the
serve command dropping templates and fallback pools in token mode, profiles
keeping and refusing the token, the difficulty shown from compact bits, and
the dashboard and status texts. Not yet run with hardware; no token is
registered.

## Merge-mined tokens under Job Declaration (2026-10-10)

#### PR #42

A Job Declaration client now merge-mines: every local job on the pool's
plan, the declared coinbase and the custom job's outputs carry the
commitment as output 0 and the tickets (worth 0) after the pool's outputs,
which the pool's rule already allowed. The leaves bind the miner's own
script (the pool's first output) and the pool's whole donation rate as the
split, since no donation work runs under Job Declaration; the claim worker
checks a win against those terms (the win now carries its job's plan).
`--merge-test-token` is accepted at a pool with `--job-declaration` and
refused without it. A pool token whose rates exceed 100% is refused, so the
payout math cannot underflow.

Evidence: a template test (the declared coinbase equals a local coinbase,
the custom job's outputs pass the pool's rule, the leaves bind the plan's
terms, no change without tokens), a plan test for the rate guard, flag and
refusal tests, and a loopback run in both modes where a device's share at a
Pickaxe pool wins the test token's Case A and Case B, both are proven at
the client, the pool accepts the custom job (and, Full-Template, its node's
`validateblocktemplate` passes the declaration), and the block with the
commitment reaches the client's node and, Full-Template, the pool's. Not yet
run live.

## Job Declaration interop with SRI clients (2026-10-10)

#### PR #42

A Pickaxe pool now takes an SRI-shaped Full-Template declaration: BIP141's
marker and flag and the one 32-byte witness item SRI's job factory gives
every coinbase are left out (any other witness is refused as "segwit
coinbase"), so the block the pool's node checks and submits is the one the
client's devices mined (the txid never covered the witness). A zero-value
witness-commitment `OP_RETURN` passes as an output the payout rule does not
count. `docs/job-declaration.md` gains a table of who works with whom.

Evidence: host tests for the strip (the stripped shape equals the plain one,
any 32-byte item is accepted, no item, two items or a 31-byte item are
refused, a witness without the marker fails as outputs) and a pool session
where an SRI-shaped declaration is validated as a block without a witness
whose merkle root holds and whose declared job rebuilds the same coinbase.
Not yet run against SRI's `jd-client` binary.

## GPU mining goes back to the miner's node (2026-10-10)

#### PR #42

After a fall back to the Fulcrum servers, GPU mining now goes back to the
miner's own node by itself. A watch (`src/job_source.rs`) asks the node again
on its own thread, 15 seconds after the fall back and then waiting twice as
long after each failure up to 5 minutes (2 minutes for a syncing node, 30 for
a refused login, another network or a node without the PHOTON state). A node
must be on the mined network, synced and at most a block behind the mined
job, and then follow the mined baton (no scan while it is current). The
supervisor hands the node over only with no winner, claim or winner refresh
in flight, through the same path that installs any source. While the node
is down, the cadence refresh no longer calls the node on the supervisor
thread (before, an 8 s connect or a scan of up to 180 s there blocked
refreshes and claims, and the session never went back).

The dashboard's Source row names the source ("your node", "Fulcrum", or
"Fulcrum (node down 4m, next try in 25s)") with a Node row giving the reason
while it is down; the status file has `job_source` (kind, label, and while
the node is down its time, next try, trouble and reason, never its address);
the event log and JSON events name the node going down and coming back;
`mine watch` shows the source's label. Reconnect asks the node at once.

Evidence: host tests for the watch's waits and hand-over (held while a claim
is in flight), the status texts, the error classes, the node checks (another
network, syncing, lagging, a node that cannot follow the baton), a refresh
on a fake Fulcrum server that leaves a configured node untouched while it is
down, the Source and Node rows, the events, the status file and the watch
header. Not yet run live (stopping the Chipnet node needs the operator's OK).

## Per-rig GPU health on the coordinator (2026-10-10)

#### PR #42

Each rig's report now carries its GPUs: name, engine and device, rate,
temperature, fan, power, status, winners, rejected winners and last error
(the search now counts rejected winners per GPU, and the rig samples its
GPUs' telemetry). The coordinator keeps at most 64 per rig, with text
stripped of control characters and capped and readings range-checked, plus
the rig's version and when it was last heard. The dashboard lists each GPU
under its rig, keeping a GPU that needs a look (not mining, 85°C or above,
rejected winners, an error) when space is short; the status file and
`mine watch` carry the same. Reports stay compatible both ways.

Evidence: host tests for the report's round trip and both directions of
compatibility, a worst-case report of 64 GPUs fitting one rig frame, the
cleaning, matching search stats and telemetry to each GPU, a coordinator
keeping 64 of 70 cleaned GPUs with the rig's version and last-heard time,
the dashboard rows and priorities, the status file and the watch rows. Not
yet run live with two rigs.

## Home Fulcrum servers over plain TCP (2026-10-10)

#### PR #42

GPU mining can take PHOTON jobs from a home Fulcrum server that offers only
plain TCP (Umbrel, StartOS): `--fulcrum tcp://umbrel.local:50001`, or the
same in the setup's Fulcrum list. Requests go one JSON-RPC line each way
(notifications skipped) with the WebSocket path's timeouts. A `tcp://`
server is taken only on this computer or the home network (loopback,
private and link-local ranges, Tailscale, IPv6 unique-local and link-local,
`localhost`, `.local` names), since plain TCP could be impersonated to feed a
false baton; the public TCP servers in the published catalog stay unused.

Evidence: host tests for the address rules (in the runtime and the shared
sources), a TCP Fulcrum fixture giving the PHOTON job, and the catalog using
a home TCP server but never a published public one. Not yet run against an
Umbrel or StartOS server.

## A node's chain is proven by its fork block (2026-10-10)

#### PR #42

A Bitcoin (BTC) node reports the same chain name ("main") as a BCH node, and
testnet4 shares Chipnet's genesis, so the chain name alone could let the
ASIC server build BTC templates. A node's block at the fork height must now
be the network's fork block: BCH's UAHF block (478,559) on mainnet, Chipnet's
block 115,252 on Chipnet. The ASIC server's template source checks it once
per node (again after a switch to another node) and refuses another chain
("node is on Bitcoin (BTC), not Bitcoin Cash" or "testnet4, not Chipnet");
GPU mining's return to the node checks it once per node too and reports the
node as on another network. A node below the fork height cannot be told yet
and is treated as syncing.

Evidence: host tests for a template source refusing a node with another
fork block and asking the right one once per node, the node check (BCH
passes and is not asked again, BTC refused, a young node syncing), and the
GPU watch classing the refusal as another network. Not yet run against a
BTC or testnet4 node.

## Saved nodes are checked in the setup (2026-10-10)

#### PR #42

Opening the setup's BCH node list now checks every saved node of the network
(at most 16, each on its own thread, and again after a node is saved). Each
line says what its check found, never with the node's login: the client,
sync height and "follows PHOTON"; "cannot follow PHOTON (no gettxout)" for a
node such as Knuth, whose BCH ASIC templates still work; a refused RPC login;
no answer; another network ("on Chipnet, not Mainnet"); or another chain by
its fork block ("Bitcoin (BTC), not Bitcoin Cash", "testnet4, not Chipnet").
A node without getnetworkinfo now reads as "BCH node" instead of failing.

Not yet: the wider search of this computer (Fulcrum, ZMQ and other ports),
`bitcoin.conf` logins and the Fulcrum list's tip beside a node's height.

Evidence: host tests for the checks against scripted nodes (BCH following
PHOTON, BTC, the other network, testnet4's fork block, no gettxout and no
getnetworkinfo, an unreachable node without its login in the text) and the
setup's list showing each saved node's result.
