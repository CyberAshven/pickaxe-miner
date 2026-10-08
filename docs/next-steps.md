# Next steps after the portable miner

Decided on 2026-10-05, after #22 (the portable engine and every GPU of a
machine in one miner) and its promotion to master in #31. The owner subsequently
approved the shared BCH/SV2 and distributed GPU work together in PR #38. Keep
that work in the existing PR; implementation and evidence are tracked in
[implementation-status.md](implementation-status.md).

## Decisions

- **Order**: (1) full BCH Stratum V2, so a miner points their ASIC at Pickaxe;
  (2) GPU scaling across machines; (3) the remaining gaps.
- **Where hashes go**:
  - P2Pool is a first-class destination and the intended default, designed in
    from the start: pooled payouts without a pool operator.
  - Stratum V2 pools as a failover list, using the miner's own block templates
    where a pool offers Job Declaration. The model is Gupax for Monero, which
    bundles P2Pool with the miner and keeps lists of nodes it pings, picks from
    and fails over between.
  - Solo on the miner's own node.
- **Networks**: Chipnet and mainnet are one code path with a network switch;
  new work is tested on Chipnet first. Network differences live in data
  (deployments, ports, address prefixes, server lists), never in separate code.
- **Node connection**: the automatic Fulcrum server list stays the default,
  since many miners will not run a node. Using one's own node becomes a guided
  step: detect local nodes, read cookie authentication, show name, version and
  sync height, and fall back to the server list when the node stops answering.
- **ASIC mode** offers two choices: an ASIC-exclusive token (SAFA-style), or
  BCH plus every merge-minable SHA-256 token at once.

## Step 1: point your ASIC at Pickaxe (BCH Stratum V2)

- Pickaxe serves Stratum V2 to the miner's devices: SV2 firmware such as
  Bitaxe's connects directly, and SV1 firmware (most home ASICs, such as the
  Avalon Nano) connects through a translator.
- Block templates come from the miner's own node: BCHN through Pickaxe's
  full-template JSON-RPC client initially, with GBT-light and a Knuth native
  Stratum V2 provider adapter remaining integration targets.
- Pickaxe checks shares, submits blocks, and shows each device's hash rate,
  shares and rejects in the dashboard.
- Built on the Stratum V2 reference implementation's Rust crates (encryption,
  framing, channels), not a new implementation.
- BCH specifics to respect: canonical transaction order (every template is a
  full template, and any job declaration keeps that order), no segwit or
  witness commitment, templates sized to BCH's adaptive block size limit, a
  target per block from ASERT, and CashAddr payout scripts.
- Testing: the reference implementation's CPU mining device first, then a real
  ASIC.

## Step 2: GPU scaling

- `pickaxe coordinator` holds the payout address, the chain connections, the
  claim journal and the claim relay; it is the only process that talks to the
  chain.
- `pickaxe rig` runs on each machine with every local GPU, as `mine` does
  since #22, takes pushed jobs from the coordinator and returns verified
  winners. No nonce ranges are needed: every GPU signs with its own key.
- On a winner the coordinator re-verifies it, pauses every rig, journals and
  broadcasts the claim, and sends the successor job.
- Rigs connect over the same Stratum V2 transport as step 1; one dashboard
  shows every rig and GPU; rigs fail over to a backup coordinator.

Status, first slice in #32: `mine --rigs-listen ADDR` makes the normal miner
the coordinator, and `mine --coordinator ADDR --coordinator-key KEY` runs a
rig. Jobs and winners travel over the Stratum V2 Noise transport as a Pickaxe
extension (type `0x5043`, JSON payloads). The coordinator checks every rig
winner (job, Schnorr signature, digest, proof of work, a search key separate
from the payout) before its claim path, which rebuilds the transaction from
the coordinator's own job and payouts, so a rig cannot redirect a reward.
Rigs switch to each successor job as soon as the coordinator starts it. The
coordinator's dashboard and `--json` status list each rig (name, GPUs, rate,
winners, time connected). A rig can be given backup coordinators, tried in
order. Rigs ship in the default build. Still to come: a live test across
machines. The portable engine fixes from issue #30 are in #32 too (see
Follow-ups from #22).

## Step 3: the gaps

- ASIC-exclusive tokens: SAFA-style tokens hash an 80-byte commitment laid out
  like a block header, so SHA-256 ASICs can grind it with header-only jobs.
- Merge mining: tokens whose covenant accepts BCH proof of work (a commitment
  in the coinbase and a merkle path to the header), so one ASIC mines BCH and
  every merge-minable token at once. This needs a covenant design and a VM
  proof with token authors first; no miner offers it yet.
- The guided own-node setup.
- The Stratum V2 pool failover list with Job Declaration, and coinbase payouts
  (sv2-spec PR #203) where pools support them.
- P2Pool: the sharechain kept beside the miner's own node.

## What already exists

Researched on 2026-10-05.

- [bchn-sv2-bridge](https://github.com/danhaus93-ops/bchn-sv2-bridge): a Stratum
  V2 template provider for unmodified BCHN over JSON-RPC, tested on mainnet. It
  keeps Job Declaration off because declaring jobs without canonical order
  produces invalid blocks.
- [LoneStrike's BCH apps](https://github.com/danhaus93-ops/umbrel-bch-apps): the
  reference implementation's pool patched for BCH, connected to that bridge,
  with a BCH solo pool built on ASICseer pool, BCHN and Fulcrum, for Umbrel.
- [Knuth](https://github.com/k-nuth/kth): a BCH node developing native Stratum V2
  support. July 2026 merges include framing
  ([#534](https://github.com/k-nuth/kth/pull/534)) and connection-setup messages
  ([#540](https://github.com/k-nuth/kth/pull/540)); these are protocol building
  blocks, not proof of a complete template provider. Keep the adapter in scope,
  but use BCHN for live Chipnet validation until Knuth interoperability is ready.
- [ckpool](https://github.com/ckolivas/ckpool): Stratum V2 for pools and solo,
  with a Job Declaration server; Bitcoin only.
- [SoloFury](https://solofury.com/blog/stratum-v2-bitcoin-cash-solo-mining/):
  Stratum V2 for BCH solo mining, closed source.
- [Bitaxe firmware](https://github.com/bitaxeorg/ESP-Miner): a native Stratum V2
  client.
- [Stratum V2 UI](https://github.com/stratum-mining/sv2-ui): the official Umbrel
  setup wizard for pool, solo and Job Declaration mining (Bitcoin), a model for
  Pickaxe's setup screens.
- [Stratum V2 reference implementation](https://github.com/stratum-mining/stratum)
  and its [applications](https://github.com/stratum-mining/sv2-apps); the
  [coinbase payouts extension](https://github.com/stratum-mining/sv2-spec/pull/203)
  (open).
- BCH pools: [ASICseer pool](https://github.com/ASICseer/asicseer-pool) and
  EloPool (ckpool forks); P2Pool for BCH in
  [jtoomim's P2Pool](https://github.com/jtoomim/p2pool).
- [SAFA](https://bitcoincashresearch.org/t/safas-a-sha256-asic-minable-automated-token-market/2123):
  a SHA-256 ASIC-minable token proposal for BCH.

## Open questions

- Build on the AGPL-3.0 bridge, or write Pickaxe's own template provider client
  on the reference crates.
- Whether Bitaxe's Stratum V2 client accepts header-only jobs, which
  SAFA-style tokens need.
- When to start the merge-minable covenant design with PHOTON's author.

## Follow-ups from #22

- Done in #35: a winner is claimed as soon as the winning GPU's batch ends,
  without waiting for the other GPUs' batches (up to about 350 ms on wgpu).
- Intel GPUs: an HD 520 user found three portable-engine problems (issue #30);
  #32 fixes all three (wgpu 30's crash on older Vulkan drivers through wgpu's
  unreleased upstream fix, the self-test on T2 engines, and throttled mining
  stuck at its first batch size), awaiting a run on the HD 520.
- Hardware not yet run: discrete AMD cards on native HIP, Linux GPUs and Apple
  Silicon.
- Telemetry for GPUs mined through wgpu on Windows when no vendor tool is
  installed.
- A longer test of an integrated GPU beside a discrete one (it added about 2% in
  a 20-second test).
- The next feature release bumps the version.
