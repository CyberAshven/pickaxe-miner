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
| **Job Declaration client** (PR #42) | Mine at a Pickaxe pool with your own node's templates (`--job-declaration coinbase`); devices fall back to the pool's own jobs when the pool refuses them. See [job-declaration.md](job-declaration.md). |
| **Job Declaration server** (PR #42) | Accept miners' own templates at a public pool (`--accept-job-declaration coinbase`), checking each custom job's coinbase pays the pool's fee and the donation. See [job-declaration.md](job-declaration.md). |
| **Template Provider server** (PR #42) | Serve this node's templates to SV2 pools, Job Declaration clients and P2Pool over SV2 Template Distribution (`--tp-listen`), and relay the blocks they find. See [Serving templates to a pool](#serving-templates-to-a-pool-template-distribution). |

GPU CashToken mining stays on its existing path. This work adds an ASIC-facing
SV2 surface; it does not replace the portable GPU product bar.

## Crate choice

Build on the [stratum-mining](https://github.com/stratum-mining/stratum) reference
crates, not a from-scratch protocol:

- Prefer the umbrella **`stratum-core`** (currently **0.6.0** on crates.io),
  which re-exports `binary_sv2`, `codec_sv2`, `framing_sv2`, `mining_sv2`,
  `template_distribution_sv2`, and siblings.
- Focused crates remain an option if a thinner set is needed later.

These dependencies sit behind the Cargo feature **`stratum-v2`**, part of the
default build since #32 (which also uses it for the encrypted link between a
GPU coordinator and its rigs). The browser and portable builds choose their
own features and leave it out.

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
| **Pickaxe's own template server** (PR #42) | `serve --tp-listen` hands this node's full templates to SV2 pools and P2Pool, the other direction; see [Serving templates to a pool](#serving-templates-to-a-pool-template-distribution). |

## Open decision: AGPL bridge vs own TP client

**Initial implementation:** an in-tree full JSON-RPC template provider, pinned
to the source node. GBT-light and a Template Distribution client remain
pending; the server side is in PR #42.

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

The default native build includes the server (`cargo build --locked`).

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
row per device with its label, status, rate over 5 minutes and over
1 hour, the device's own report, accepted and rejected shares, reject rate, last
share, current share difficulty, protocol and latest issue. Tab switches to the
overview, the arrow keys, PgUp/PgDn, Home and End move the highlighted row
(PR #42; the overview scrolls), Enter opens its Device panel, and `a` opens
Advanced settings. An
address-only worker still gets its own row and unique work. Both rates come
from validated shares after a 30-second warm-up; they are statistical
estimates, so compare the 1-hour rate with the device's own figure. Up to 64
closed sessions remain visible. JSON `device_details` contains the same rows,
including last-share age and SV1-local reject counts.

#### PR #42

A device on the local network keeps one row and one label through
reconnects. When it connects again from the same local address, it takes over
its offline row and the number in its label (`rig1 #12` stays `rig1 #12`).
This also works when the device comes back before Pickaxe sees its old
connection drop, as after a power cut: until then the new connection shows a
new number beside the old row, and when the old connection closes, the new one
takes its number. A short extra connection that a firmware opens
beside its mining one, closed within a minute, leaves no row. A device
connecting from this computer or from a public address gets a new row and label
on each reconnect. The address itself is never shown or written to the JSON
status.

Behind a Tailscale subnet router, a VPN gateway or carrier-grade NAT, many
devices share one address. A device there that goes offline stays as an
offline row while the others mine. But when one of them reconnects, it takes
over every offline row on that address and the number of the one that went
offline last, so another device's offline row can disappear and a number can
move to a different device.

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
this computer's address to point devices at, and names each device by the
worker name its owner set (`rig1`, or `rig1` in `ADDRESS.rig1`; an address
alone is never shown); `i` opens Connection info, with
every address (your network and Tailscale), SV1 for stock firmware and SV2
with the authority key in the address (`stratum2+tcp://HOST:3336/KEY`), each
ready to copy. SV1 has no encryption, so use it on a trusted network.

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

Several nodes: add more to the node list (node1, node2, …). The server starts
on the first that gives a synchronized template and moves to the next, in
order, whenever its node gives none; it tries the new node at once, and the
failed one waits at the back of the list. The dashboard and `watch` show
"node 2 of 3" and the status counts the moves. Found blocks are saved whole,
so a block waiting for a reply goes to whichever node is in use. The block
journal belongs to the network and payout, not to one node, so the server
also starts when its first node is down; a journal written by an older
build opens while its node is still configured.

#### PR #42

On the workers page, the arrow keys, PgUp/PgDn, Home and End move a
highlighted row. Enter (or `c`) opens the Device panel for it, online or
offline, so a device that stopped mining can be restarted from its offline
row. The highlight stays on its device when rows re-sort (online rows come
first) and when the device reconnects. The table scrolls to keep it in view.

The panel shows the device's model, firmware and power, and how it is reached:
"local network", "Tailscale/CGNAT" or "IPv6 local". It never shows the
device's address, not even inside a device's reply or error. An offline row
shows how long it has been offline and uses the address its device last
connected from.

While the panel opens, Pickaxe identifies the device in the background, for at
most ten seconds. It then lists what that make and firmware support through
asic-rs: Restart, Pause and Resume mining, and blink or stop blinking its
light to find it. Avalons also get one work level down or up, through
Pickaxe's own Canaan `ascset worklevel` commands, within the device's own
range (asic-rs sets power in watts, which Avalon work levels are not). Restart
on any Avalon is Canaan's own reboot (`ascset 0,reboot,0`). asic-rs's Avalon
restart only restarts the mining program, and its Avalon Home Q has none. A
device asic-rs does not identify offers Pickaxe's own Restart (Canaan's
`ascset` reboot or Bitaxe's restart endpoint) and work levels.

The panel also sets the fan and the power where the device allows it:

- **Fan speed:** `a` for automatic (the device follows its temperature), or a
  speed typed in percent. Through asic-rs on stock Antminer, ePIC and Proto
  firmware. On an Avalon through Canaan's `ascset 0,fan-spd` (15% to 100%), and
  on a Bitaxe or NerdAxe through AxeOS's settings.
- **Power:** a limit in watts where asic-rs sets one (Braiins, VNish,
  WhatsMiner, Auradine, Proto, SealMiner, and ePIC through its tuning), or the
  Low, Normal and High modes (stock Antminer). An Avalon gets Canaan's work
  modes when it lists `workmode`, beside its work levels.

An Avalon lists the settings it accepts in its `ascset 0,help` reply, which
the panel reads once when it opens. A setting it does not list is not offered.
When the list cannot be read, the setting is offered and the device decides. A
Bitaxe accepts changes only from its own local network: through Tailscale or a
VPN it answers that it refuses, and the panel says so.

**Pools.** "Pools…" reads the device's pools when you ask, marks the one that
is this server and which one it mines on now, and shows each pool's worker
name, never a payout address. `n` puts a new pool first: type its address with
its port (as in `stratum+tcp://pool.example:3333`), the worker and, if the pool
wants one, a password. `h` puts this server first again, with the worker the
device already used here. The device's other pools stay as backups, as many as
it holds (three on most, two on a Bitaxe). Before anything is sent, the panel
shows the list the device will hold, what is dropped, and what the change
means: while pool 1 works, the device stops mining on this server, so its
shares, any block it finds, merge-mined token wins and the donation from its
work go to that pool. Pools are written through asic-rs on most makes, with
Canaan's `setpool` on Avalons (it needs the Avalon's web login, entered with
`l`, and the device reboots), and through AxeOS on a Bitaxe (it restarts).
Firmwares that do not report pool passwords keep the backups with password
`x`; the confirmation says so.

**Device logins.** Most firmwares answer Pickaxe with their default login.
When a device refuses an action because its owner changed the login, the panel
says so: press `l`, type the device's login (the username comes filled in with
the firmware's default; the password shows as dots), and the action is sent
again with it. `l` works at any time, for example for a stock Antminer whose
password was changed, which cannot even be identified without it. The login is
saved only after the device accepts it, in `<config>.sv2-logins.json` beside
the server's config, readable by this computer's owner alone. It is used only
for that device and only for the firmware it was saved for, since another
device may get the address later. It is never shown, logged or written to the
status file.

Each action and setting needs a confirmation, goes only to a device on the
local network or Tailscale, and shows the device's reply. The panel takes
every key, so `q` there never stops the server. Esc goes back.

An offline row's address may since belong to another device, for example
after its DHCP lease passed on. So for an offline row Pickaxe looks at the
device there afresh, and offers actions only if it identifies as the same make
and model the row reported while online. When that cannot be compared (the
device is not one asic-rs identifies), actions are offered only within ten
minutes of the row going offline. Two devices of the same model cannot be
told apart this way.

The panel refuses to control a worker in these cases, and says why:

- Its address is not known: it connected from this computer or from a public
  address.
- Its address is shared. Behind a Tailscale subnet router, a VPN gateway or
  carrier-grade NAT, every device has the gateway's address, and commands
  would reach the gateway, not the device. An address counts as shared when
  two connections from it are each a minute old, or once two devices there
  each kept mining for a minute after the other connected. That second mark
  stays, even after one of them goes offline, until no row from the address
  remains. A firmware's short extra connection beside its device does not
  count. Control such devices from their own network.
- Several connections from its address are all under a minute old, for
  example right after the server starts. Pickaxe cannot tell yet whether they
  are one device. Open the panel again a minute later.

`stratum-v2 watch` has the same highlight and Device panel. It acts on the
devices itself, never through the server: it takes each worker's address from
`<config>.sv2-devices.json`, which the server keeps beside its config,
readable by the server's user alone, and uses the server's saved device
logins. So run watch as the server's user (for example `sudo -u pickaxe
pickaxe stratum-v2 watch --config …`); otherwise the panel says it cannot read
the list. Every address in that file is checked again before use, so an edited
file cannot point Pickaxe at a public address. Device addresses never go into
the status file, which everyone can read and which service logs print.

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
A pool's one-line SV2 address carries its key, so `--upstream
stratum2+tcp://HOST:PORT/KEY` needs no `--upstream-key`; the setup's Pool row
takes the same line and fills the key. An SV1 pool address
(`stratum+tcp://…`) is refused: Pickaxe joins SV2 pools only and translates
SV1 for the devices on this side.
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
and the device reconnects for a new channel. A pool must allocate at least
eight miner extranonce bytes per channel; the adapter gives each device four
bytes of its own as extranonce1 and lets it roll four as extranonce2, and puts
the pool's channel prefix into the coinbase part the device receives.

The donation applies in pool mode too, as mining time: since the pool builds
the blocks, Pickaxe cannot add a coinbase output, so the whole BCH donation
setting (1.5% by default, 0% to 100% in Advanced settings or `--donation`) is
that share of each device's mining time, mined at the same pool under the
donation address on a second channel. A 10-minute cycle per device decides
which channel feeds it (9 seconds at 1.5%); a switch sends the channel's
difficulty and a clean job, so the device never reconnects or falls back to
its own backup pools. At 0% no donation channel is opened. A pool that refuses
the second channel, for example one whose usernames are accounts rather than
addresses, keeps the device mining on its own channel, and the workers page
shows why. Native SV2 devices such as Bitaxe can connect to SV2 pools
themselves.

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

### Serving templates to a pool (Template Distribution)

#### PR #42

A server with its own node can also hand that node's block templates to SV2
pools, Job Declaration clients and P2Pool over SV2 Template Distribution, so
they mine on this node's templates without its RPC login. It is off by
default:

```text
pickaxe_miner stratum-v2 serve --chipnet --config chipnet.json --tp-listen 0.0.0.0:48442
```

In the setup it is the **Serve templates** row under Advanced, for solo mining
and an ASIC pool, on the network's template port: 8442 on mainnet, 48442 on
Chipnet. Joining a pool cannot serve templates: that pool's come from its own
node. The listener uses the mining listener's key, so a client pins the same
authority. Connection info (`i`) lists its addresses, numbered for copying,
and SRI's configuration lines:

```toml
[template_provider_type.Sv2Tp]
address = "192.168.0.160:48442"
public_key = "<the server's authority key>"
```

What a client gets:

- `SetupConnection` for protocol 2, version 2 and no flags. Anything else is
  refused with its error code, echoing unsupported flags, and the session
  closes.
- After its `CoinbaseOutputConstraints` (within 10 seconds): on a new parent,
  a future `NewTemplate` and at once its `SetNewPrevHash` (the node's current
  time, `nBits`, and the target `compact(nBits)`); on the same parent, a
  current `NewTemplate` only. A lease renewal or a merge-mined token's change
  sends nothing. A change of constraints sends the template again, at most
  once a second.
- The coinbase prefix is the BIP34 height push alone. A BCH template asks for
  no coinbase outputs and has no witness commitment, so the client's outputs
  take the whole `coinbase_tx_value_remaining`.
- Template ids are `max(last + 1, Unix milliseconds)`, rising across restarts.
- A template is withheld, and counted, when 153 bytes plus the client's
  reserve do not fit beside its transactions within the block size limit.
- `RequestTransactionData` returns the transactions in block (CTOR) order. A
  template whose parent was replaced answers `stale-template-id`, an unknown
  one `template-id-not-found`, and one beyond SV2's single frame (16,777,215
  bytes or 65,535 transactions, possible under ABLA) `template-too-large`.
- A client can name its 16 latest templates; those on a replaced parent
  answer for 10 seconds more. At most 8 clients connect at once. A client
  frame over 64 KiB, or more than four transaction-data requests a second,
  closes the session.

A solution (`SubmitSolution`) is assembled into the template's block. SRI's
pools submit a coinbase with BIP141's marker, flag and one 32-byte witness
item; Pickaxe strips exactly those and refuses any other witness, since BCH has
no witnesses and the block carries the plain coinbase. The coinbase script
must begin with the height push sent, the version may differ only in the
version-rolling bits, and the header must meet the target. A block that passes
is saved in `<config>.sv2-relay-blocks.json`, owner-only beside the block
journal and never mixed with it, and then submitted and retried like this
server's own blocks until the node answers. A restart, even without
`--tp-listen`, still submits what that file holds. One block per parent is
kept. A solution that fails Pickaxe's checks still goes to the node, at most
one every 10 seconds, and is never saved: a bug in those checks must not drop
a pool's only submission. The overview shows the template clients, the
templates sent and withheld, and the relayed blocks; the status file's
`template_server` has the same counts and never an address.

Sigops are not counted on BCH, which counts SigChecks while scripts run, so
the client's sigops reserve is ignored. Pickaxe's own Template Distribution
client, for templates from another Pickaxe or a native template provider, is
the next step.

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
the target it met. While merge-mined tokens are mined (none is registered
yet), a device's target is made easier, up to the easiest token target that
needs no block. It is never more than 15 times easier than vardiff's (about 5
shares a second), and never easier than the device allows. Within that range
firmware sends every hash that wins a token. A token target easier than that
is mined best effort. Vardiff counts and estimates only at its own target.
Only the first solved block on each parent is saved; later
solutions on the same parent count as shares. SV1 adapter rejects count in the
shared dashboard totals; separate adapter/native connection error fields can
both describe the same disconnected session. A Template Distribution client,
distributed rigs and pool routing are still pending.
BCH uses an adjustable donation, defaulting to 1.5%: one third of it is mining
work and two thirds is the block reward (0.5% and 1% at the default). The
dashboard shows the total and both parts, each rounded up to two decimals; the
combined effect of the two parts is slightly below the total (1.495% at 1.5%).
In the setup, ASIC mining and Join a pool keep their server options under
**Advanced** on the settings page (Enter on its header opens it): the
donation, the start difficulty, the SV1 and SV2 ports, and for Join a pool the
backup pools (each `stratum2+tcp://HOST:PORT/KEY`, used in order when the pool
fails) and the username at the pool (your payout address by default). A
saved profile keeps them.

`--donation 2` selects 2%; the dashboard's Advanced settings (`a`) change the
saved setting in 0.5% steps from 0% to 100%. Merge-mined tokens share this
setting: two thirds as a split in each token claim, and one third through the
same donation work. GPU token policies remain separate.
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
