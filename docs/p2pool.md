# BCH P2Pool, managed by Pickaxe

#### PR #53

Status: `pickaxe p2pool` installs the BCH P2Pool node from P2Pool's own
releases, keeps it updated, runs it with your node and payout, restarts it
when it exits, and shows a light for P2Pool and one for the BCH node. Not
yet: Pickaxe's ASIC server preferring this P2Pool and falling back to solo
mining while it is not green, with the same lights on its dashboard, and the
setup's P2Pool choice (still "coming soon"); that is the next change.

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
  `zmqpubhashblock` from its `bitcoin.conf`. P2Pool needs them.
- **Payout:** `--address` pays P2Pool's first jobs, before any share exists.
- **SV2 key:** made once in `p2pool/sv2-authority.key` (owner-only), as
  Pickaxe's own pool key is. P2Pool never makes keys; its public key goes in
  the file and the secret in the environment. Devices on this computer reach
  P2Pool at `stratum2+tcp://127.0.0.1:3361/<public key>`, shown on the screen.
- **Ports:** Stratum V1 3360 and V2 3361 and the API 46860 on this computer
  only; P2P 6860 on every interface. They differ from P2Pool's defaults so a
  node run by hand and Pickaxe's ASIC server can run beside it.
- **Share chain:** mini by default (shares 256 times easier, for smaller
  miners) or main: `pickaxe p2pool set --share-chain main`. Each network and
  chain has its own store in `p2pool/`.
- **Peers:** `pickaxe p2pool set --peer /ip4/HOST/tcp/PORT` (repeatable;
  `--no-peers` forgets them). With peers, P2Pool waits for their share chain
  before taking miners. With none, it starts its own and takes miners at
  once (it would otherwise wait for good).

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

## Commands

| Command | Does |
|---|---|
| `pickaxe p2pool run [--no-update]` | runs P2Pool with the screen (see above) |
| `pickaxe p2pool update [--check]` | installs the newest release, or only says whether one is out |
| `pickaxe p2pool rollback` | goes back to the version before |
| `pickaxe p2pool status` | the installed and newest versions and the settings |
| `pickaxe p2pool set ...` | `--auto-update on/off`, `--share-chain main/mini`, `--peer ADDRESS`, `--no-peers` |

## Not in this change

- The ASIC server's use of it: devices to this P2Pool while it is green,
  solo to the node while it is not, the two lights on the ASIC dashboard,
  and the setup's P2Pool choice. P2Pool needs each miner's share address
  (`,p2p=`); the code that derives it is on `feat/p2pool-alpha-upstream`.
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

## Checks

- `p2pool::http`: URLs (ports, IPv6, no logins), chunked and sized bodies,
  short and oversized ones refused, a redirect followed.
- `p2pool::release`: the newest release with this platform's archive and
  checksum (drafts and incomplete releases skipped, pre-releases taken), the
  `.sha256` forms, a real archive installed and rolled back, a swapped
  download and an archive without the binary installing nothing. Ignored
  test, run on 2026-10-19: P2Pool's live release list through Pickaxe's own
  HTTPS client has `v0.0.1-alpha` with the Windows archive and its checksum.
- `p2pool::launch`: the configuration (node, ports, chain, store, logs,
  peers and the wait that follows them) with no secret in it, the secrets in
  the environment, the SV2 keys in the form P2Pool parses, the lights from
  P2Pool's log lines and API, a real child process restarted after it exits
  with the reason, and one turning green and stopping on request.
- `p2pool::screen`: both lights, the version and update line, where devices
  connect and the log.
