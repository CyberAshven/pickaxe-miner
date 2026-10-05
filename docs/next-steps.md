# Next steps after the portable miner

Planned on 2026-10-05, after #22 (portable engine and every GPU of a machine
in one miner) and its promotion to master in #31. This is a plan for review,
not shipped behavior. Each phase becomes its own pull request, and each starts
with a short design check before code.

## Where Pickaxe is now

- One miner drives every GPU of a machine: CUDA, HIP and wgpu engines, one
  job, one claim path, per-GPU status and restart after a GPU failure.
- PHOTON work never overlaps between GPUs without any nonce bookkeeping: each
  GPU signs with its own random key, and every key has its own 2^32 nonces.
- The supervisor polls the live baton every 500 ms, verifies every GPU winner
  on the host, journals a claim before broadcasting it, and relays it to more
  Fulcrum servers.
- The browser miner runs the same engine on WebGPU.

## Goals

1. Solo miners first. One person's GPUs, up to farms of 100 GPUs on many
   machines, mine as one: one job, one dashboard, no competition between their
   own GPUs, and their own node.
2. Pickaxe stays a general miner: GPU tokens today, ASIC tokens and BCH itself
   next.
3. Merge-mine CashTokens with BCH. No other miner can do this yet.
4. Pools stay optional: a solo miner can switch to a Stratum V2 pool and still
   build block templates with their own node.

## Phase 1: rigs and farms

One coordinator mines for many machines.

- `pickaxe coordinator` holds the payout address, the Fulcrum and node
  connections, the claim journal and the claim relay. It is the only process
  that talks to the chain.
- `pickaxe rig --coordinator HOST:PORT` runs on each machine. It mines with
  every local GPU, as `mine` does since #22, but takes jobs from the
  coordinator and returns host-verified winners.
- Jobs are pushed, not polled: the coordinator sends a new generation to
  every rig the moment the baton changes, the way stratum pushes work rather
  than getwork polling for it.
- No nonce ranges are needed, unlike ethminer's per-GPU ranges or
  xmrig-proxy's split nonces, because every GPU already signs with its own key.
- On a winner the coordinator re-verifies it, pauses every rig, journals and
  broadcasts the claim, then sends the successor job. A second winner for the
  same baton is stale and only counted.
- Transport: Stratum V2 (Noise-encrypted, binary framing, heartbeats), from
  the Rust reference implementation. Rigs and SV2 ASIC firmware connect to the
  miner's own coordinator the same way.
- Dashboard: rigs and their GPUs in one view (rate, temperature, power,
  errors, last seen), in the TUI and as JSON for farm tools such as Hive OS.
- Failover: rigs reconnect to a backup coordinator; a restarted coordinator
  resumes from its claim journal before it sends work.
- Rigs hold no keys of value: the payout is an address, and the search keys
  are ephemeral and hold no funds.

First step: the job and winner messages, a two-rig test on one machine, then
the laptop plus a second PC on mainnet.

## Phase 2: Stratum V2

- Solo, the default: Stratum V2 is how the miner's own rigs and ASICs talk to
  their own coordinator, which builds block templates from their own node.
- Nodes do not offer the miner side of SV2 (template provider, Job Declaration
  client, non-custodial payouts), so the coordinator provides it. Templates come
  from the miner's own node over its JSON-RPC `getblocktemplate`, with long
  polling or ZMQ block notifications; no node IPC is needed.
- Pool, optional: the miner picks a pool from a list of pools that support
  SV2 with Job Declaration. Pickaxe declares jobs built from the miner's own
  node (transactions, coinbase and token commitments), and the pool only counts
  shares and pays. The list is ordered for failover. No BCH pool offers SV2
  today, so this mode waits for one.
- Pickaxe's own role stays either way: the token layer (covenant mining and
  claims), the GPU engines, the coordinator, and merge mining on top of BCH
  work. Merge mining needs the miner's own templates, which both modes keep.

Pools may be a transition: with Job Declaration a pool only counts shares and
pays, and a sharechain (phase 5) does both without custody. Pooled payouts
without a pool therefore come from P2Pool, not from pool support.

## Phase 3: BCH ASIC solo mining

- A stratum server for SHA-256 ASICs, fed by `getblocktemplate` from the
  user's own BCH node, for solo mining in the style of ASICseer and ckpool
  solo.
- ASIC list, hash rate, rejects and temperatures in the same dashboard as the
  GPUs.
- Kaspa's stratum bridge, which turns a node's work into stratum work for
  ASICs, is a useful reference for the shape.

## Phase 4: merge-mining tokens with BCH

The goal no other miner has. A token covenant would accept proof of BCH
mining work, as auxiliary proof of work does for Namecoin.

- The block template carries a commitment to each token's job (in the
  coinbase); a BCH share that also meets a token's target becomes a token
  claim, with a merkle path from the coinbase to the header.
- This needs covenants designed for it: they must verify a BCH header and a
  merkle branch in script within BCH's VM limits. That is protocol work with
  token authors (PHOTON's author first), before any miner code.
- Pickaxe's part: build the templates (phase 3 and SV2 Job Declaration),
  track every token's job, and claim each token a share qualifies for.

First step: a written covenant design and a VM proof, as done for PHOTON's
layouts in `tools/reward-policy-vm`.

## Phase 5: P2Pool (maybe)

A decentralized pool for BCH and merge-mined tokens, after phases 3 and 4.
With P2Pool every participant runs the sharechain: here the coordinator keeps
it beside the miner's own BCH node, and the miner's rigs and ASICs connect to
their coordinator as in phase 1. Using someone else's P2Pool node would make it
an ordinary pool, so that is not a goal. It helps BCH ASIC solo miners most,
whose blocks are rare; PHOTON wins already come every few minutes across the
network. DATUM, where miners build their own templates and the pool only
pays, is the nearer model to study.

## Follow-ups from #22

- Claim latency on rigs with slow GPUs: a winner is claimed once every GPU
  has finished its current batch (at most about 350 ms on wgpu). Claiming as
  soon as the winning GPU's batch ends would remove that wait.
- Hardware not yet run: Intel GPUs (#30's HD 520 on Vulkan and DirectX 12),
  discrete AMD cards on native HIP, Linux GPUs and Apple Silicon.
- Telemetry for GPUs mined through wgpu on Windows when no vendor tool is
  installed.
- The integrated Radeon added about 2% beside the RTX 5070 Ti in a 20-second
  test; a longer test should show whether laptops gain or only heat up.
- An optional hosted browser miner, if wanted.
- The next feature release bumps the version; 0.0.3's downloads were rebuilt
  from master on 2026-10-05.

## Decisions for the operator

Agreed on 2026-10-05: solo miners first; rigs and ASICs talk Stratum V2 to the
miner's own coordinator and node; pools are an option with the miner's own
templates (Job Declaration); P2Pool means running the sharechain beside one's
own node.

Still open:

1. Order: rigs and farms first (recommended), or SV2 first.
2. Whether to open the merge-mining covenant design with PHOTON's author now.
3. Which pools to list for the optional pool mode, once their SV2 and Job
   Declaration support for BCH is checked.
