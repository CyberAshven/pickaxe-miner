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
| **Own JSON-RPC template client** | Current implementation feeds the mining server from BCHN full `getblocktemplate` / `submitblock`. GBT-light remains pending. |
| **Knuth native TP (future integration)** | Upstream has merged SV2 framing and message building blocks. Those changes do not establish a complete, interoperable template provider. |

## Open decision: AGPL bridge vs own TP client

**Initial implementation:** an in-tree full JSON-RPC template provider, pinned
to the source node. GBT-light and native Template Distribution remain pending.

**Chipnet test baseline:** use BCHN through the in-tree full-template client.
This keeps deployment to Pickaxe plus the node, without an external bridge.
Knuth remains an integration target; live Knuth testing is deferred until its
native provider is ready for interoperability checks. Upstream
[#534](https://github.com/k-nuth/kth/pull/534) adds plaintext framing and
[#540](https://github.com/k-nuth/kth/pull/540) adds connection-setup messages;
neither is evidence of a complete production template provider.

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
An optional SV1 listener translates older firmware through the same encrypted
SV2 path using the reference translation library.
The tests include a local TCP CPU miner and an independent block decoder.
An Avalon Nano 3 has also mined confirmed Chipnet blocks, including a header
matched through two independent public servers. This does not establish native
SV2 firmware compatibility or compatibility with every ASIC; see the evidence
and remaining gates in `implementation-status.md`.

Build the native application with `cargo build --locked --features stratum-v2`.
The feature remains opt-in. No release is published by this change.

Commands (using a local Pickaxe config with the selected network, a valid
payout address, and the operator's node RPC connection):

```text
pickaxe_miner stratum-v2 status
pickaxe_miner stratum-v2 check-node --chipnet --config chipnet.json
pickaxe_miner stratum-v2 serve --chipnet --config chipnet.json
pickaxe_miner stratum-v2 serve --chipnet --config chipnet.json --sv1-listen 127.0.0.1:3333
```

`check-node` is read-only and prints a redacted template summary. `serve`
binds to `127.0.0.1:3336` unless `--listen IP:PORT` selects a LAN interface.
The dashboard shows the public authority key that devices must pin. Its
private key is created beside the config as `chipnet.sv2-key`, protected with
the same owner-only file mechanism as saved RPC credentials. Invalid existing
keys cause an error instead of silent identity replacement. Press `q` to stop;
`--no-tui` provides JSON status and Ctrl+C shutdown.

Use `--sv1-listen LAN-IP:3333` for SV1 firmware on your trusted mining LAN;
configure the ASIC with `stratum+tcp://LAN-IP:3333` and a worker identity (an
address alone is accepted; a worker suffix is optional). This
local listener uses the server's configured payout, never an address supplied
by the ASIC username. SV1 has no encryption or access authentication; its
authorize method registers a worker identity, and does not validate a password.
The adapter's connection to the local SV2 server is encrypted and pins its key.
It supports version-mask negotiation, subscribe/authorize in either order,
and forwards success only after upstream share validation. Node block acceptance
is reported separately. Jobs, pending replies, input size and setup/partial-I/O
time are bounded. The initial adapter handles one extended channel per device
and the server's immediate acknowledgements; it is not a general upstream pool
translator. Avalon Nano 3 has been tested; Antminer and native Bitaxe SV2
validation remain pending. Each listener currently caps connections at 64;
this is not a claim of 1,000-device capacity.

The full-template provider checks network, synchronization, tip identity,
CTOR, transaction bytes/IDs, header target and adaptive block size. It revokes
work on an unavailable source. A block is counted accepted only when the
source node returns success or its matching header has positive confirmations;
a share acknowledgement does not establish that.

#### PR #38

Solved full blocks are saved beside the config in `chipnet.sv2-blocks.json`
before the device receives its share acknowledgement. The journal is private,
locked against concurrent writers and bound to the node endpoint, network and
configured payout. Changing RPC credentials does not change that binding.
Keep this file when upgrading or restarting: pending blocks are retried from
their original bytes, including after the chain tip changes. Do not delete it
to resolve a startup error or change its source while work is pending.

A missing node reply is pending, not rejected or accepted. Retries back off
from one to 30 seconds; an exact BCHN `duplicate` response means the original
block is already accepted. Explicit permanent validation failures are counted
separately. Pending removal and its completion receipt are one atomic update,
so recovery does not count the same outcome twice. Accepted/rejected block
counters persist; share and retry counters describe the current server run.
The legacy JSON `blocks_unconfirmed` field aliases the current pending count.

The journal holds up to 64 pending blocks with 64 MiB of aggregate raw block
data and 1,024 recent completion receipts. These are storage bounds, not BCH
consensus limits. Storage failure or exhaustion stops the server before an
unsaved block is acknowledged. Corrupt or mismatched state is preserved and
causes a startup error. Abrupt-exit, restart, lost-reply and failed-write tests
use synthetic solved blocks; actual node/ASIC evidence is recorded separately.

The initial share difficulty is 4096, reduced when the network has easier work
so firmware does not discard valid Chipnet blocks. A device target limit that
cannot accommodate this work is rejected. Vardiff, individual device rates,
Knuth TP, distributed rigs and pool routing are still pending.
Current BCH coinbases pay the configured mining address; integrating the
compiled BCH donation policy remains separate from existing token policies.
The successful Chipnet experiments do not complete those remaining capabilities.

The authority encoding follows the [SV2 security specification, section
4.7](https://github.com/stratum-mining/sv2-spec/blob/main/04-Protocol-Security.md#47-url-scheme-and-pool-authority-key).
