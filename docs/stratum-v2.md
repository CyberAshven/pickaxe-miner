# BCH Stratum V2 design check

Short design lock for step 1 of [next-steps.md](next-steps.md): Pickaxe serves
Stratum V2 so a miner can point their ASIC at Pickaxe. Chipnet first; mainnet
shares the same code path with a network switch.

## Roles Pickaxe will play

| Role | Purpose |
|---|---|
| **Template Provider client** | Pull full block templates from the miner's own node (BCHN or Knuth) and keep them fresh. |
| **Mining server** | Serve SV2 mining channels to devices (Bitaxe native SV2 and SV1 via translator). Check shares, submit blocks. |
| **SV1 translator** | Accept Stratum V1 firmware (Avalon Nano and most home ASICs) and translate to the SV2 mining server. Required in this implementation scope. |

GPU CashToken mining stays on its existing path. This work adds an ASIC-facing
SV2 surface; it does not replace the portable GPU product bar.

## Crate choice

Build on the [stratum-mining](https://github.com/stratum-mining/stratum) reference
crates, not a from-scratch protocol:

- Prefer the umbrella **`stratum-core`** (currently **0.6.0** on crates.io),
  which re-exports `binary_sv2`, `codec_sv2`, `framing_sv2`, `mining_sv2`,
  `template_distribution_sv2`, and siblings.
- Focused crates remain an option if a thinner set is needed later.

Gate those deps behind Cargo feature **`stratum-v2`** (not in default features).
This branch wires optional `stratum-core` behind that feature so default CI
stays green; enable with `cargo check --features stratum-v2` / `cargo test
--features stratum-v2 --lib stratum_v2`.

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

**Initial implementation:** an in-tree full JSON-RPC template provider, pinned
to the source node. GBT-light and native Template Distribution remain pending.

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
3. Exercise the SV1 translator with the owner's existing ASIC firmware on
   Chipnet; verify block acceptance and propagation before mainnet use.

## Implementation checkpoint

PR #38 includes the shared BCH/SV2 and distributed GPU work. The remaining
scope is tracked in [implementation-status.md](implementation-status.md);
the earlier scaffold-only exclusions no longer apply.

The current implementation has reference Noise/framing, setup negotiation,
standard and extended channels, version rolling, unique coinbases, share
verification, node-pinned full-block submission, and an aggregate dashboard.
The tests include a local TCP CPU miner and an independent block decoder.
They do not establish live Chipnet, upstream CPU-device, or ASIC interoperability.

Build the native application with `cargo build --locked --features stratum-v2`.
The feature remains opt-in. No release is published by this change.

Commands (using a local Pickaxe config with the selected network, a valid
payout address, and the operator's node RPC connection):

```text
pickaxe_miner stratum-v2 status
pickaxe_miner stratum-v2 check-node --chipnet --config chipnet.json
pickaxe_miner stratum-v2 serve --chipnet --config chipnet.json
```

`check-node` is read-only and prints a redacted template summary. `serve`
binds to `127.0.0.1:3336` unless `--listen IP:PORT` selects a LAN interface.
The dashboard shows the public authority key that devices must pin. Its
private key is created beside the config as `chipnet.sv2-key`, protected with
the same owner-only file mechanism as saved RPC credentials. Invalid existing
keys cause an error instead of silent identity replacement. Press `q` to stop;
`--no-tui` provides JSON status and Ctrl+C shutdown.

The full-template provider checks network, synchronization, tip identity,
CTOR, transaction bytes/IDs, header target and adaptive block size. It revokes
work on an unavailable source. A block is counted accepted only when the
source node returns success; a share acknowledgement does not establish that.

The initial share difficulty is 4096 (bounded by the device's maximum target).
Vardiff, individual device rates, SV1 translation, Knuth TP, distributed rigs,
pool routing and a durable block-submission retry journal are still pending.
Current BCH coinbases pay the configured mining address; integrating the
compiled BCH donation policy remains separate from existing token policies.
Do not treat this checkpoint as a completed or live-validated ASIC product.

The authority encoding follows the [SV2 security specification, section
4.7](https://github.com/stratum-mining/sv2-spec/blob/main/04-Protocol-Security.md#47-url-scheme-and-pool-authority-key).
