# BCH P2Pool, managed by Pickaxe

#### PR #53

Status: `pickaxe p2pool` installs the BCH P2Pool node from P2Pool's own
releases, keeps it updated, runs it with your node and payout, restarts it
when it exits, and shows a light for P2Pool and one for the BCH node.
#### PR #61: the ASIC setup's P2Pool choice is ready: join a P2Pool node, or
have Pickaxe run your own P2Pool node with your BCH node and mine at it,
with its lights on the ASIC dashboard (see "Mining at it from the ASIC
setup").
#### Pool modes: `stratum-v2 serve --own-p2pool --public` hosts a pool for
other people with P2Pool inside, with your fee, signature and P2Pool
donation in P2Pool's configuration, fallback pools, and nodes without ZMQ
(see "Pool modes").

## The approach

Pickaxe treats P2Pool the way Gupaxx treats Monero's P2Pool: as a separate
program it installs, configures, starts and watches, never as code it
integrates. P2Pool's logic (its share chain, PPLNS window, payouts and
stratum servers) stays P2Pool's; Pickaxe only calls it. A P2Pool change
reaches miners as a new P2Pool release, which Pickaxe installs as it would
any dependency update.

## Installing and updating

Releases come from [CyberAshven/BitcoinCash-p2poolv2](https://github.com/CyberAshven/BitcoinCash-p2poolv2/releases),
pre-releases included (it has published only those so far, and GitHub's
"latest release" skips them). Pickaxe takes the newest release that has this
platform's archive and the `.sha256` file beside it:

| Platform | Archive |
|---|---|
| Windows x86_64 | `p2poolv2_node-x86_64-pc-windows-msvc.zip` |
| Linux x86_64 | `p2poolv2_node-x86_64-unknown-linux-gnu.tar.xz` |
| Linux arm64 | `p2poolv2_node-aarch64-unknown-linux-gnu.tar.xz` |
| macOS Apple Silicon | `p2poolv2_node-aarch64-apple-darwin.tar.xz` |

It downloads both over HTTPS, refuses an archive whose SHA-256 is not the
published one, unpacks it with the system's `tar` (Windows 10 and later,
Linux and macOS have it) into `p2pool/versions/<tag>/`, finds `p2poolv2`
there and records it in `p2pool/installed.json` with the version before it.
`pickaxe p2pool rollback` goes back to that version (and again returns).

**Automatic updates are on by default**, as Claude updates itself: `pickaxe
p2pool run` installs a newer release before starting P2Pool, and keeps the
installed one if the update fails. With them off (`pickaxe p2pool set
--auto-update off`, or `a` on the screen), the screen says a newer release is
available and `u` (or `pickaxe p2pool update`) installs it; P2Pool restarts
on it.

What the check proves: the file is the one published beside its checksum in
that repository, over HTTPS. It does not prove who built it: the checksum is
not signed, and `v0.0.1-alpha`'s Windows archive was built locally rather
than by P2Pool's release workflow. Building every platform in that workflow
and publishing one signed or attested checksum file is the next step on
P2Pool's side (release tooling only).

## Running

`pickaxe p2pool run --network chipnet --node-rpc https://HOST:PORT --address
bchtest:...` writes `p2pool/<network>.toml` at each start:

- **Node:** `--node-rpc`'s URL. Its login (from the URL, the environment or
  a saved cookie file, as Pickaxe finds it for itself) goes to P2Pool in its
  environment, since P2Pool has no cookie support; it is never written to
  the file.
- **Block notices:** `--node-zmq`, or for a node on this computer
  `zmqpubhashblock` from its `bitcoin.conf`. #### Pool modes: without
  them (`--node-zmq off`, or none found), P2Pool hears of blocks on the
  node's P2P port instead (`node_p2p_address`, see "Pool modes").
- **Payout:** `--address` pays P2Pool's first jobs, before any share exists.
- **SV2 key:** made once in `p2pool/sv2-authority.key` (owner-only), as
  Pickaxe's own pool key is. P2Pool never makes keys; its public key goes in
  the file and the secret in the environment. Devices on this computer reach
  P2Pool at `stratum2+tcp://127.0.0.1:3361/<public key>`, shown on the screen.
- **Ports:** Stratum V1 3360 and V2 3361 and the API 46860 on this computer
  only; P2P 6860 on every interface (on this computer only when standalone).
  They differ from P2Pool's defaults so a node run by hand and Pickaxe's
  ASIC server can run beside it.
- **Share chain:** mini by default (shares 256 times easier, for smaller
  miners) or main: `pickaxe p2pool set --share-chain main`. Each network and
  chain has its own store in `p2pool/`.
- **Peers:** as "Finding other P2Pool servers" below says. With peers
  to dial, P2Pool waits for their share chain before taking miners. With
  none, it starts its own and takes miners at once (it would otherwise wait
  for good).

When P2Pool exits, Pickaxe starts it again after 5, 10, 30, then 60 seconds,
and back to 5 once it ran for a minute. P2Pool exits at start when the node
is not ready; the screen then says why ("the BCH node is not ready: ...").
Quitting stops P2Pool by ending its process. On Windows P2Pool runs in a job
object that ends with Pickaxe, so it also stops when Pickaxe crashes or is
closed by force, and frees its ports; on Linux and macOS it keeps running
then until stopped.

The secrets reach P2Pool as `P2POOL_STRATUM__SV2_AUTHORITY_SECRET_KEY`,
`P2POOL_BITCOINRPC__USERNAME` and `P2POOL_BITCOINRPC__PASSWORD`: `P2POOL_`
and the key path with `__` between levels, as P2Pool's loader reads them.
(P2Pool's sample config names `P2POOL__STRATUM__...`, which it ignores.)

## Finding other P2Pool servers

The owner's names: a **Non-custodial Pool** is a P2Pool running alone on its
own share chain; **P2Pool v2** is one linked with other servers.

P2Pool dials only the `dial_peers` of its configuration: there are no
Bitcoin Cash P2Pool DNS seeds, and a Bitcoin Cash node never asks any. After
its first connection it finds the others itself (libp2p identify and
Kademlia), so one server that answers is enough. Pickaxe gives it that
first contact from three lists (`src/p2pool/peers.rs`):

- **Seeds:** compiled into Pickaxe per network, `SEEDS_MAINNET` and
  `SEEDS_CHIPNET`. **None yet.** The owner adds one as a libp2p address,
  `/ip4/A.B.C.D/tcp/PORT`, `/ip6/.../tcp/PORT` or a DNS name
  `/dns4/pool.example.com/tcp/PORT` (looked up at each start, so the server
  can move), without `/p2p/<id>`; the next release carries it.
- **Yours:** `pickaxe p2pool set --peer ADDRESS` (repeatable, same forms;
  `--no-peers` forgets them). Always dialed, first.
- **Known:** the servers P2Pool dialed out to, read from its API's
  `GET /peers` (outbound peers only: an inbound peer's address is the port it
  connected from). Kept in `p2pool/peers-<network>.json`: addresses and
  times only, at most 64, each dropped after a week without an answer.

**The check**, as Gupaxx checks its node list: before each start (and on
`pickaxe p2pool peers --check`), one TCP connect to each entry, DNS names
looked up first, 2 s each, 16 at once, 10 s in all. All of your peers are
dialed first, then the seeds and known servers that answered, fastest
first, up to 8 in all. When none answers, all of them are written anyway, so P2Pool keeps
trying them. Nothing is checked on a timer while mining.

**Standalone** (`pickaxe p2pool run --standalone`, the Non-custodial Pool):
no peers dialed, the P2P port on `127.0.0.1` only so discovery cannot link
another server in, and no waiting for a share chain.

`pickaxe p2pool peers` lists the three lists with each entry's last check;
`pickaxe p2pool status` shows their counts; the screen's Chain line says
"alone (standalone)" or how many peers the start dials, and "no seeds yet"
while the network has none.

## The lights

| | Green | Yellow | Red | Grey |
|---|---|---|---|---|
| P2Pool | its API answers and its SV2 port listens | starting; the node is syncing or gives no block template; the share chain is syncing | it exited; starting again in N s | stopped |
| Node | answers on the network's chain, synced | answers, still syncing | no answer, wants its login, or another chain | not checked yet |

The node light uses the same check as the setup's node list. The screen also
shows the installed version and updates, the share chain and peers, where
devices connect, restarts, and P2Pool's newest log lines. Keys: `u` install
an update, `a` automatic updates on or off, `r` restart P2Pool, `q` quit.
`--no-tui` prints each change of either light instead.

## Mining at it from the ASIC setup

#### PR #61

ASIC mining, **Join a pool**, then **Pool type: P2Pool**. The choices are a
list; Pickaxe never moves devices from one to another by itself.

| Your templates | What runs | You give |
|---|---|---|
| off: join a P2Pool node, which builds the jobs | Pickaxe's server mines at that node over SV2 | its HOST:PORT and authority key |
| your node's: Pickaxe runs your own P2Pool v2 node with it | Pickaxe installs, updates and runs P2Pool as above, linked with other P2Pool servers, and mines at it | your BCH node (and its ZMQ block notices or P2P address) |

- **Payout:** P2Pool pays shares to a key, so the payout must be a P2PKH
  (`q…`) address. The server's identity at P2Pool is
  `<payout>,p2p=<share address>`, the share address made from the payout's
  key (`pickaxe stratum-v2 share-address` prints it). Pickaxe's donation
  channel names the donation key's own share address the same way.
- **Your own P2Pool node:** the setup asks for your BCH node; with ZMQ
  notices off it shows the node's P2P address (see "Pool modes"); Advanced
  has P2Pool's own donation and fallback pools, and the main rows your
  peers. Its
  server command is `pickaxe stratum-v2 serve
  --own-p2pool --sv1-listen 0.0.0.0:3333 ...` with the profile's node,
  notices and payout: it installs or updates P2Pool as `pickaxe p2pool set`
  says, starts it, mines at `stratum2+tcp://127.0.0.1:3361/<its key>`, and
  stops it when the server stops (P2Pool's own job object on Windows).
  Templates and Job Declaration are off: P2Pool builds the jobs, and the
  pool sets the difficulty.
- **Lights:** the ASIC dashboard's top line shows P2Pool's and the node's
  lights (as on the P2Pool screen, with a waiting update noted) for your own
  P2Pool node; the pool's light when joining one (green while a worker
  mines through it, red when the pool refused the last connection); and the
  node's light when the server builds its own templates (solo, and Job
  Declaration with the pool's light beside it).
- **When P2Pool is not green:** devices wait on it, or with
  `--fallback-pool` go to a fallback pool (see "Pool modes"); there is no
  switch to solo. Choose solo mining in the setup to mine solo.
- **Profiles:** saved as joining a pool, with your own P2Pool's settings
  (`p2pool`) beside it; that it is P2Pool, and your own, is also kept in
  `profile-choices.json` beside the profiles.
- Joining a public P2Pool node, your own node's templates can be mined
  there too (Job Declaration, PR #65): see
  [job-declaration.md](job-declaration.md#public-bch-p2pool-with-your-own-bch-node).

## Pool modes

#### Pool modes

| Mode | Who mines | A block pays |
|---|---|---|
| **Solo** | your devices, on your node | you |
| **Public Solo Endpoint** (`serve --public`) | anyone, through your server | the miner who found it |
| **Non-custodial Pool** (`serve --own-p2pool --public --p2pool-standalone`) | anyone, through your server | everyone on your server, by their work: P2Pool runs inside, on its own share chain, alone |
| **P2Pool v2** (`serve --own-p2pool --public`) | the same, linked with other P2Pool servers | everyone on the shared share chain, by their work |
| **Join a pool** | your devices, at someone else's server | as that server pays |

```text
pickaxe stratum-v2 serve --config pool.json --network chipnet \
  --node-rpc http://NODE:48332 --address bchtest:q... \
  --listen 0.0.0.0:3336 --sv1-listen 0.0.0.0:3333 \
  --own-p2pool --public --p2pool-standalone \
  --pool-fee 1 --pool-fee-address bchtest:q... --pool-tag /MyPool/ \
  [--p2pool-donation 1] [--fallback-pool stratum2+tcp://HOST:PORT/KEY ...]
```

- **The door:** miners connect to your server as to a Public Solo Endpoint
  (SV1 3333, SV2 3336 with your server's key, Connection info, the workers
  table, the Device panel). P2Pool's stratum stays on this computer.
- **Each miner under their own identity:** each device's channel at P2Pool
  opens under `<payout>[.worker],p2p=<share address>`, the share address of
  the miner's own key, never yours. SV1 devices get a channel each from the
  adapter; each SV2 connection is relayed to P2Pool on a link of its own,
  each channel under its miner's identity. P2Pool pays shares to a key, so a
  payout that is not P2PKH (`q…`) is refused when the device authorizes, or
  opens its SV2 channel (`p2pkh-payout-required`), with the reason. A `p2p=`
  in the login must be the payout's own share address.
- **Four cuts, never mixed:** the Pickaxe donation (yours to pay, as the one
  running Pickaxe: your donation setting, or a miner's `,donation=` when
  higher, never both and never lower, a typed `,pickaxe-donation=0`
  included), mined as work on a second channel under the donation key's
  share address, for SV1 devices (the adapter) and SV2 devices (the door)
  alike; P2Pool's donation (`--p2pool-donation`, your choice as its
  runner); your fee; the miner keeps the rest. A miner's `,donation=` never
  reaches P2Pool. Another Pickaxe joining through the door donates on its
  own donation channel; the door credits that channel's accepted work over
  the last hour, as a Public Solo Endpoint does (see pool.md, "Fees"), and
  takes only the rest.
- **The door's donation:** for each SV2 device channel the door opens a
  second channel at P2Pool under the donation's identity and gives the
  device that channel's jobs for the donation's share of its accepted work
  (the SV1 adapter's rule), with the pool's extranonce prefix in each job's
  coinbase part; its job ids and shares map back to the channel they came
  from, and verdicts come back under the device's own channel.
- **Your settings, in `p2pool/<network>.toml`:**

| Flag | P2Pool's key | Limit |
|---|---|---|
| `--pool-fee`, `--pool-fee-address` (with `--public`) | `fee` (basis points), `fee_address` | at most 10%; paid in the coinbase, so `--pool-fee-mode work` or `both` is refused |
| `--pool-tag` | `pool_signature` | at most 16 characters |
| `--p2pool-donation PERCENT` | `donation` (basis points) | 0 to 10 in 0.5 steps; default 1, P2Pool's own |
| `--p2pool-standalone` | its own share chain, P2P on this computer, no peers | without it, linked (P2Pool v2), as `p2pool run --standalone` (see "Finding other P2Pool servers") |
| `--node-p2p HOST:PORT` | `node_p2p_address` | see Knuth below |

- **Failover:** `--fallback-pool` works with every `--own-p2pool` mode, by
  the solo fallback's rule: once P2Pool has not been green for 3 s, its
  sessions end and devices go to the first fallback pool that takes them
  (in a pool, each miner under their own payout); they come back after
  P2Pool has been green for 30 s without a break.
- **Knuth and other nodes without ZMQ:** P2Pool connects to the node's P2P
  port as a peer and hears of each block there (`node_p2p_address`):
  `--node-p2p HOST:PORT`, by default the node's host with 8333 (mainnet) or
  48333 (Chipnet) when the node gives no ZMQ notices. P2Pool itself still
  needs BCHN: its startup check refuses a node whose `getblockchaininfo`
  reports no `chain`, or another than BCHN's name for the network, and
  checks BCH's fork block with `getblockhash`; its jobs come from BCHN's
  `getblocktemplate`, which P2Pool's RPC client notes Knuth lacks. Knuth
  needs those in P2Pool (P2Pool's change) before P2Pool runs on it.
- **Profiles:** a profile keeps `p2pool` (donation, standalone, node_p2p)
  beside its ASIC pool or join server, with fallback pools; the setup sets
  them (Run a pool, ASIC pool, Mode; see [pool.md](pool.md#a-non-custodial-pool-or-p2pool-v2)).

## Commands

| Command | Does |
|---|---|
| `pickaxe p2pool run [--no-update] [--standalone]` | runs P2Pool with the screen (see above); standalone alone on its own share chain |
| `pickaxe p2pool update [--check]` | installs the newest release, or only says whether one is out |
| `pickaxe p2pool rollback` | goes back to the version before |
| `pickaxe p2pool status` | the installed and newest versions, the settings and the peer counts |
| `pickaxe p2pool peers [--check]` | the seeds, your peers and the known ones with their last check; `--check` checks them now |
| `pickaxe p2pool set ...` | `--auto-update on/off`, `--share-chain main/mini`, `--peer ADDRESS`, `--no-peers` |

## Not in this change

- `v0.0.1-alpha` predates P2Pool's `/miner` API and its template-refresh
  fix; a new release brings them to everyone running it.
- A newer release that will not start is not rolled back by itself; the
  screen shows it restarting, and `pickaxe p2pool rollback` returns.
- `--node-zmq` takes a host on this computer or the home network, Tailscale
  addresses (100.64.0.0/10) included, but not Tailscale names (`*.ts.net`);
  give such a node's Tailscale address.

## Live check (2026-10-19)

On the operator's Windows PC, in a scratch configuration folder:

- `pickaxe p2pool update` read the release list, downloaded `v0.0.1-alpha`'s
  Windows archive (19.3 MB), found its SHA-256 equal to the published
  `.sha256` and to GitHub's own digest
  (`b744b129…cb25`), unpacked it and found `p2poolv2.exe` one folder down;
  `status` then said up to date.
- `pickaxe p2pool run --network chipnet` with the operator's Start9 BCHN
  (over Tailscale, its login passed in the environment): the node light went
  green ("Bitcoin Cash Node 29.1.0 · synced at height 328496") and P2Pool's
  green ("running · share chain tip 0 · 0 peers": its own new mini chain);
  its API answered on 46860.
- The first run found P2Pool ignoring the `P2POOL__...` names from its sample
  config ("missing configuration field bitcoinrpc.username"); the supervisor
  restarted it with backoff and showed that reason. The names were corrected
  as above. It also found a headless line printed each second of the
  countdown; lines are now printed per change.
- Closing Pickaxe by force left P2Pool running and holding its ports; with
  the job object it now ends with Pickaxe.

## Live check: your own P2Pool node from the ASIC server (2026-10-19)

#### PR #61

On the operator's Windows PC, with a release build of this branch and the
P2Pool already installed above: `stratum-v2 serve --network chipnet
--own-p2pool --sv1-listen 127.0.0.1:3399` with the Start9 BCHN (login in the
environment), its ZMQ notices and a throwaway Chipnet payout.

- The server started P2Pool `v0.0.1-alpha` (no newer release) and mined at
  it: the dashboard said "Pool your P2Pool node (v0.0.1-alpha)", with the
  lights "P2Pool ● v0.0.1-alpha · running · share chain tip 0 · 0 peers" and
  "Node ● Bitcoin Cash Node 29.1.0 · synced at height 328550".
- A stand-in SV1 device (subscribe and authorize, no hashing) was answered
  at once and got P2Pool's jobs at difficulty 10,000: seven in 45 seconds.
  Its row recorded no refusal, of its channel or the donation's.
- The first build wrapped the two lights at 118 columns and pushed the
  header's last line out; they now go one per line when they do not fit.
- Stopping the server, and closing its terminal, stopped P2Pool and freed
  its ports; the operator's other P2Pool nodes and miner were untouched.

Not shown: shares and a block (P2Pool's share difficulty is beyond a CPU),
and a real ASIC at it.

## Checks

- `p2pool::http`: URLs (ports, IPv6, no logins), chunked and sized bodies,
  short and oversized ones refused, a redirect followed.
- `p2pool::release`: the newest release with this platform's archive and
  checksum (drafts and incomplete releases skipped, pre-releases taken), the
  `.sha256` forms, a real archive installed and rolled back, a swapped
  download and an archive without the binary installing nothing. Ignored
  test, run on 2026-10-19: P2Pool's live release list through Pickaxe's own
  HTTPS client has `v0.0.1-alpha` with the Windows archive and its checksum.
- #### Pool modes: `p2pool::launch` (the operator's fee, donation and
  signature in the TOML and P2Pool's limits; `node_p2p_address` without
  ZMQ), `stratum_v2::door` (two miners reach P2Pool each under their own
  identity, through the SV1 adapter and the SV2 door; an owner-only node
  keeps one identity; failover to a fallback pool and back; an SV2 device
  mines the donation's jobs for its share, its shares reach the right
  channel, and a joining Pickaxe's visible donation is credited),
  `stratum_v2::command` (fee mode work/both and a long signature refused),
  `config` (the profile's P2Pool settings) and `cli`.
- `p2pool::launch`: the configuration (node, ports, chain, store, logs,
  peers and the wait that follows them) with no secret in it, the secrets in
  the environment, the SV2 keys in the form P2Pool parses, the lights from
  P2Pool's log lines and API, a real child process restarted after it exits
  with the reason, and one turning green and stopping on request.
- `p2pool::screen`: both lights, the version and update line, linked or
  alone, where devices connect and the log; the `peers` listing.
- `p2pool::peers`: the dial list (standalone empty, yours first, answering
  before silent, the cap, all of them when nothing answers, DNS names as
  written) against local listeners; the known-peers file (round trip, the
  week, the cap, addresses and times only); learning from `GET /peers`.
- `p2pool::launch`: standalone and linked configurations (listen address,
  peers, wait), the check made once per start, and the supervisor keeping
  the peers P2Pool dialed out to.
- #### PR #61: `tui::tests::an_asic_can_join_a_pool_or_a_p2pool_node` and
  `tui::tests::an_asic_can_mine_at_its_own_p2pool_node` (the rows, Start's
  checks, the serve command for each, the profile and its kept choices), and
  `stratum_v2::command::tests::the_dashboard_lights_show_the_pool_and_the_node`.
