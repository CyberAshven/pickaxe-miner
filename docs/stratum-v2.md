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
CI also builds a pinned, unmodified SRI CPU mining device and checks authority
pinning and two successor blocks on one standard-channel connection against
synthetic BCH templates. Build instructions and the exact source/binary evidence
are in `implementation-status.md`; this does not test a live native SV2 ASIC.
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
The dashboard opens on a workers table, laid out like a pool's worker list: one
row per session with its generated label, status, rate over 5 minutes and over
1 hour, the device's own report, accepted and rejected shares, reject rate, last
share, current share difficulty, protocol and latest issue. Tab switches to the
overview, Arrow/Page keys scroll, and `a` opens Advanced settings. An
address-only worker still gets its own row and unique work; reconnecting creates
a new session label. Both rates come from validated shares after a 30-second
warm-up; they are statistical estimates, so compare the 1-hour rate with the
device's own figure. Up to 64 closed sessions remain visible. JSON
`device_details` contains the same rows, including last-share age and SV1-local
reject counts.

"Device says", Temp and Fan are the device's own report. Every 15 seconds
Pickaxe asks each connected device on the local network (private, link-local,
100.64.0.0/10 and IPv6 local addresses, never a public address), all devices
in parallel, from two sources:

- [asic-rs](https://github.com/256foundation/asic-rs) (256 Foundation,
  Apache-2.0) identifies the make, model and firmware once and reads each
  device's own API: Antminer, Whatsminer, Avalon, Bitaxe, NerdAxe, Braiins OS,
  Vnish, LuxOS, ePIC, Auradine and more. It supplies the model, firmware and
  power draw, and the rate, temperature and fans for makes Pickaxe's own reader
  does not know.
- Pickaxe's own read-only reader (the CGMiner API's `summary` and `estats` on
  port 4028, or Bitaxe's `/api/system/info`, three seconds per device) leads
  where it is closer to the device's own app: the 5-minute rate (asic-rs reads
  Avalon's 1-minute rate), the hottest temperature, the fan as the device
  states it, and Avalon Nano power (asic-rs reads the Nano's input voltage,
  27.56 V, as 2,756 W). It also answers alone for a device asic-rs cannot
  identify, which is asked again after five minutes.

Pool settings (their worker names are often payout addresses), MAC
addresses, serial numbers and host names are never collected. Device
addresses are used only for these queries and never appear in the dashboard
or JSON. The overview page (Tab) shows each device's model and power.

The setup screen starts the same server: choose ASIC, then "BCH + all
merge-mined tokens", and set a payout address and your BCH node. It listens on
the local network (SV1 on port 3333, SV2 on 3336) and the workers page shows
this computer's address to point devices at. SV1 has no encryption, so use it
on a trusted network.

Your BCH node: the setup's BCH node list looks for Bitcoin Cash Node on this
computer (`127.0.0.1:8332` on mainnet, `127.0.0.1:48332` on Chipnet) and offers
it with its version and sync height; Enter saves it. BCHN answers once its
`bitcoin.conf` has `server=1` (and `chipnet=1` for Chipnet). With no
`rpcpassword` set, BCHN writes a login cookie at every start and Pickaxe reads
it from BCHN's default folder (`%APPDATA%\Bitcoin` on Windows, `~/.bitcoin` or
the service's `/var/lib/bitcoind` on Linux, `~/Library/Application
Support/Bitcoin` on macOS, with `chipnet` inside it for Chipnet), so no
password is needed; `PICKAXE_NODE_RPC_COOKIE` names a cookie file elsewhere. A
node on another computer needs its RPC login in the address
(`http://USER:PASSWORD@HOST:PORT`) or in `PICKAXE_NODE_RPC_USER` and
`PICKAXE_NODE_RPC_PASSWORD`. `check-node`, the dashboard and `watch` show the
node's client and version, and a node that cannot be used is named with its
reason, such as a refused login or a node still synchronizing.

On the workers page, `c` opens controls for the top row's device, with its
model, firmware and power. It lists what that make and firmware support
through asic-rs (Restart, Pause and Resume mining, blink or stop blinking its
light to find it) plus one work level down or up on Avalon (Pickaxe's own
Canaan `ascset worklevel` commands, within the device's own range; asic-rs's
power limit is in watts, which Avalon work levels are not). A device asic-rs
has not identified offers Pickaxe's own Restart (Canaan's `ascset` reboot or
Bitaxe's restart endpoint) and work levels. Each action needs a confirmation,
goes only to a device on the local network, and shows the device's reply. The
read-only `watch` view has no controls.

A server running without a screen, for example as a service with
`--no-tui --json`, saves the same status once a second beside its config
(`chipnet.sv2-status.json` for `chipnet.json`). On that machine,
`stratum-v2 watch` with the same network flag and `--config` shows the
server's workers table, read-only: it never connects to the server or changes
anything, and `q` leaves the server running. The status file holds no payout
addresses or credentials; reading a service's state directory may need
`sudo`. The header shows "Server not updating" when the saved status is older
than five seconds.

The dashboard also shows the public authority key that devices must pin. Its
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
time are bounded. The adapter handles one extended channel per device. Avalon
Nano 3 has been tested; Antminer and native Bitaxe SV2 validation remain
pending. Each listener currently caps connections at 64; this is not a claim
of 1,000-device capacity.

### Pool mode: SV1 devices at a remote SV2 pool

Solo mining on your own node stays the default. To mine at a Stratum V2 pool
instead, such as another Pickaxe server or a BCH SV2 pool, give the pool's
address and authority key:

```text
pickaxe_miner stratum-v2 serve --config mainnet.json --sv1-listen 0.0.0.0:3333 \
  --upstream POOL-HOST:PORT --upstream-key POOL-AUTHORITY-KEY [--upstream-user IDENTITY] \
  [--upstream BACKUP-HOST:PORT --upstream-key BACKUP-AUTHORITY-KEY ...]
```

Repeat `--upstream` and `--upstream-key`, in the same order, for backup pools.
Each device takes the first pool that completes the handshake, the setup and
the channel, so a pool that is down, or one whose certificate fails, is
skipped; a device that reconnects starts again from the first pool. All pools
share the identity. If every pool fails, the device's row shows the last
pool's reason.

No node runs and no SV2 listener opens; SV1 devices point at
`stratum+tcp://LAN-IP:3333` as usual. Each device gets its own encrypted SV2
connection to the pool, pinned to the pool's key, and an extended channel opened
with `--upstream-user`: an account or worker name, or for a solo pool your
payout address with an optional `.worker` suffix. Without `--upstream-user` it
is the configured payout address. The identity is never printed or saved in
the status file. The channel declares 1 TH/s, and pools set their first
difficulty from that.

Pools may acknowledge shares in batches, so in pool mode a device gets its
reply once the adapter has checked and forwarded the share, and the workers
page counts the pool's own verdicts as they arrive. A pool that changes a
channel's extranonce or asks for a reconnect closes that device's connection,
and the device reconnects for a new channel. SV1 firmware needs an 8-byte
extranonce2; a pool that allocates another size is refused with a clear reason.
The donation does not apply in pool mode, since the pool builds the blocks and
pays (`--donation` is refused with `--upstream`). Native SV2 devices such as
Bitaxe can connect to SV2 pools themselves.

Evidence, 2026-10-08: Pickaxe's own server as the pool, over TCP with Noise
and a host name (two blocks mined, both counted by the adapter), and SoloFury's
BTC SV2 endpoint, read-only with a throwaway identity: setup accepted, extended
channel opened with a 4-byte extranonce prefix and 8-byte extranonce2,
difficulty 1024, and a first job delivered to an SV1 device stand-in.
SoloFury's BCH SV2 endpoints run CashStratum, which signs its certificate with
format version 1; the SV2 spec requires 0 and requires clients to refuse other
versions, so Pickaxe reports "upstream certificate version is not SV2's" there
until CashStratum fixes it
([cashstratum/cashstratum#3](https://github.com/cashstratum/cashstratum/issues/3)).

The full-template provider checks network, synchronization, tip identity,
CTOR, transaction bytes/IDs, header target and adaptive block size. It revokes
work on an unavailable source. A block is counted accepted only when the
source node returns success or its matching header has positive confirmations;
a share acknowledgement does not establish that.

#### PR #38

A temporary template loss revokes jobs immediately but keeps the device
connection for up to three seconds. Recovery cleanly replaces work on the same
channel; a longer outage closes it. Submissions during that gap are rejected,
and expired job leases are never extended. The TUI and JSON report template
failure counts and a sanitized last-error category, separately from actual
device connection errors. A brief outage can still waste firmware work; it
does not count as accepted work merely because the connection stays open.

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

Each device starts at share difficulty 4096. Vardiff (SRI's reference rules)
then moves it toward about 20 shares a minute, for 1 TH/s miners and 1 PH/s
ones alike, with a floor equal to 1 MH/s. Like ckpool and P2Poolv2, it acts
only on enough evidence (72 shares, four minutes, or a silent minute) and
ignores changes under 25%, which are share luck, so the difficulty does not
jump on a short run of lucky shares. A new target is sent as SetTarget and
at once as a fresh job on the same template, so SV1 firmware receives
`set_difficulty` and a notify that keeps work in flight. When the network has
easier work, the device's target follows it down so firmware does not discard
valid Chipnet blocks, and returns on the next normal template (Chipnet allows
difficulty-1 blocks after a 20-minute gap). A device target limit that cannot
accommodate easier block work is rejected. Older jobs accept shares at the
easier of their own target and the current one, and each share is credited at
the target it met. Only the first solved block on each parent is saved; later
solutions on the same parent count as shares. SV1 adapter rejects count in the
shared dashboard totals; separate adapter/native connection error fields can
both describe the same disconnected session. Knuth TP, distributed rigs and
pool routing are still pending.
BCH uses an adjustable donation, defaulting to 1.5%: one third of it is mining
work and two thirds is the block reward (0.5% and 1% at the default). The
dashboard shows the total and both parts, each rounded up to two decimals; the
combined effect of the two parts is slightly below the total (1.495% at 1.5%).
`--donation 2` selects 2%; the dashboard's Advanced settings (`a`) change the
saved setting in 0.5% steps from 0% to 100%. Token policies remain separate.
Mainnet and Chipnet each donate to their own built-in address
(`src/donation/bch.rs`), checked by the same payout validation as the miner's.
The BCH policy is attached to each job; changing the setting never changes an
in-flight job's coinbase. Saved solved blocks retain that policy across restart,
and pre-donation journal entries replay their original bytes.

The work adapter schedules eligible channel time in ten-minute cycles with
independent random starting phases, preserving the position through template
refreshes and excluding unavailable work. ASIC hashes cannot be counted exactly
from Stratum messages: this is a time allocation, not a claim of exact measured
hash allocation or guaranteed rewards. Dispatch cadence and firmware response
can affect short intervals. Fractional satoshis stay with the miner, and the
complete coinbase retains the node's subsidy-plus-fees budget.
The successful Chipnet experiments do not complete those remaining capabilities.

The authority encoding follows the [SV2 security specification, section
4.7](https://github.com/stratum-mining/sv2-spec/blob/main/04-Protocol-Security.md#47-url-scheme-and-pool-authority-key).
