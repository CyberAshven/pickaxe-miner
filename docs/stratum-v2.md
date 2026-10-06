# BCH Stratum V2 design check

Short design lock for step 1 of [next-steps.md](next-steps.md): Pickaxe serves
Stratum V2 so a miner can point their ASIC at Pickaxe. Chipnet first; mainnet
shares the same code path with a network switch.

## Roles Pickaxe will play

| Role | Purpose |
|---|---|
| **Template Provider client** | Pull full block templates from the miner's own node (BCHN or Knuth) and keep them fresh. |
| **Mining server** | Serve SV2 mining channels to devices (Bitaxe native SV2; later SV1 via translator). Check shares, submit blocks. |
| **SV1 translator** (optional, later) | Accept Stratum V1 firmware (Avalon Nano and most home ASICs) and translate to the SV2 mining server. |

GPU CashToken mining stays on its existing path. This work adds an ASIC-facing
SV2 surface; it does not replace the portable GPU product bar.

## Crate choice

Build on the [stratum-mining](https://github.com/stratum-mining/stratum) reference
crates, not a from-scratch protocol:

- Prefer the umbrella **`stratum-core`** when the design is locked and the
  lockfile can absorb it, **or** the focused crates
  `binary_sv2` + `codec_sv2` + `framing_sv2` + `mining_sv2` +
  `template_distribution_sv2` if a thinner dependency set is needed.

Gate those deps behind Cargo feature **`stratum-v2`**. This PR only declares the
empty feature placeholder so default CI stays green; the reference crates land
in a follow-up after this design check is accepted.

License note: Pickaxe is **AGPL-3.0-only**. Reference SV2 crates are compatible
in principle; any AGPL bridge dependency is recorded as an open decision below.

## BCH specifics

Templates and jobs must respect Bitcoin Cash consensus, not Bitcoin Core
assumptions baked into some SV2 examples:

- **CTOR**: every template is a **full** template in canonical transaction
  order. Job Declaration (if/when used) must preserve that order; partial or
  reordered templates produce invalid BCH blocks.
- **No segwit / witness commitment**: coinbase and merkle construction omit
  Bitcoin-style witness commitments.
- **ASERT target**: per-block target from ASERT, not Bitcoin's nBits epoch
  schedule alone.
- **CashAddr payouts**: coinbase / payout scripts encode CashAddr (or the
  equivalent locking bytecode), not legacy-only assumptions.
- **Adaptive block size**: template size follows BCH's adaptive block size
  limit, not a fixed 1–4 MB Bitcoin envelope.

## Template sources

| Source | Notes |
|---|---|
| **bchn-sv2-bridge** | External AGPL TP for unmodified BCHN over JSON-RPC; tested on mainnet; keeps JD off because unordered JD breaks CTOR. |
| **Own JSON-RPC GBT-light client** | Wrap Pickaxe's existing `node.rs` `getblocktemplatelight` / `submitblocklight` path and speak TP (or feed the mining server directly). |
| **Knuth built-in TP** | Knuth node with merged SV2 template provider; same GBT-light RPC family. |

## Open decision: AGPL bridge vs own TP client

**Status: open.**

**Recommendation for Chipnet first:** implement Pickaxe's **own TP client**
wrapping the existing `node.rs` GBT-light calls, then feed the mining server.
Reasons: stay in-tree with AGPL-3.0-only without taking on an external AGPL
bridge as a hard runtime dependency; reuse code already proven for Chipnet
mining; keep deployment simple (miner runs Pickaxe + node, no third binary).

Revisit the bridge if a production BCHN deployment wants an unmodified-node TP
that already speaks SV2 Template Distribution on the wire without Pickaxe
owning that role.

## Test ladder

1. Reference implementation **CPU mining device** against Pickaxe's mining
   server (no ASIC required).
2. **Chipnet ASIC** (Bitaxe native SV2 preferred) against the same server with
   live templates from a Chipnet node.
3. Only then consider mainnet and SV1 translator coverage.

## Out of scope for this PR

- Live Noise handshake / encrypted SV2 sessions
- Share validation and block submission wiring
- Dashboard device hash-rate / shares / rejects UI
- Adding `stratum-core` (or sibling) crates to the lockfile
- CUDA / search hot-path changes beyond the module wire in `lib.rs`
- Donation logic

## This PR's scaffold

Native module `pickaxe_miner::stratum_v2` with roles, BCH constraint constants,
and a status report. No network I/O yet.

**CLI status today**

- Ready now: `cargo run --bin stratum_v2_status`
- Clap subcommand `pickaxe stratum-v2 status` is present behind feature
  `stratum-v2`, pending apply of `src/stratum_v2/main.rs.diff` to `src/main.rs`
  (import + match arm). After that patch, ungate the clap variant if desired so
  default builds expose the subcommand without an extra feature flag for CLI.
