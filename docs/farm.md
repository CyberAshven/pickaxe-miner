# GPU farms

One coordinator mines for many rigs. Only the coordinator holds the payout
address, talks to the chain and claims; every rig mines the coordinator's job
on all its GPUs and sends its winners back.

## How it works

- The coordinator shares its current PHOTON job with every rig over an
  encrypted link (Pickaxe's Stratum V2 Noise transport). A rig trusts only the
  coordinator whose key it was given.
- Every rig signs with its own search key, so rigs never repeat each other's
  work and need no nonce ranges.
- The coordinator checks each winner (its job, signature and proof of work)
  and builds the claim from its own job and payout, so a rig cannot redirect a
  reward. Then it moves every rig to the next job.
- A rig without its coordinator pauses and reconnects by itself; given backup
  coordinators, it tries them in order.

## The coordinator

On any computer, with or without a GPU:

```text
pickaxe mine --rigs-listen 0.0.0.0:3340 --rigs-only --address <payout> --no-tui
```

`--rigs-only` uses no GPU on that computer and never loads a GPU driver.
Without it, the coordinator also mines on its own GPUs, as a normal miner does.
It prints its key when it starts (the `rigs` line's `coordinator_key`) and
keeps it in `config.rigs-key` beside its configuration, so the key stays the
same across restarts. Keep that file private: it is the coordinator's identity.

Without `--no-tui`, the dashboard shows a Rigs row (rigs connected, their GPUs,
the farm's rate, winners and rejected winners, the listen address) and one row
per rig (name, GPUs, rate, winners, minutes connected). Its Hashrate row adds
the rigs' rate to the coordinator's own. `--json` status lines carry the same
under `rigs`.

Press `I` for Connection info: the exact command a rig runs, once for each
address other computers reach the coordinator at (its address on your network
and, when Tailscale is running, its Tailscale address), with a number key to
copy each. The `rigs` start line lists the same addresses under `connect`;
with `--no-tui` and without `--json`, the coordinator also prints each command
as a `rigs join` line.

## Rigs

```text
pickaxe mine --coordinator <coordinator address>:3340 --coordinator-key <key> --rig-name rack1-07 --no-tui
```

In the setup, choose GPU mining and set Mining to "Join a GPU pool or farm"
with the coordinator's address and key (the one-line
`stratum2+tcp://HOST:3340/KEY` fills both); Start mines as a rig.

A rig needs no address, Fulcrum server or node of its own, and mines with the
coordinator's donation setting (never below the token's minimum). It mines on
every discrete GPU (`--device` and `--include-integrated` choose others), and
prints one line per event: its GPUs at start, `connected`, each `job` and
`winner`, and a `status` line with its rate every 10 seconds; `--json` prints
them as JSON. Repeat `--coordinator` and `--coordinator-key`, in the same
order, for backup coordinators. The name defaults to the computer's name.

## A public GPU pool

A coordinator can also be a public pool for other people's rigs. Each rig
mines for its own address and its winners are claimed to it; nothing is held
or paid out later. PHOTON's claim cannot split a reward, so the operator's
fee is a share of each rig's mining time, after the donation, like the
donation itself.

```text
pickaxe mine --rigs-listen 0.0.0.0:3340 --rigs-only --address <your payout> \
  --rigs-public --rigs-fee 2 --rigs-fee-address <fee address> --no-tui
```

Rigs add `--address <their payout>`; a rig without one is turned away. For
the fee's share of each rig's time (a 10-minute clock per rig), the rig gets
the same job paying the fee address, marked by the top bit of its job number,
and its winners are checked against exactly the job it was given. Because
anyone can broadcast a PHOTON claim, a modified rig could keep its fee time
for itself; the fee, like the donation, relies on rigs running Pickaxe as
published.

## Running rigs as a service

These are examples; the live test below ran rigs as plain processes.

Linux (systemd), as `/etc/systemd/system/pickaxe-rig.service`:

```ini
[Unit]
Description=Pickaxe rig
After=network-online.target
Wants=network-online.target

[Service]
ExecStart=/opt/pickaxe/pickaxe mine --coordinator 10.0.0.2:3340 --coordinator-key KEY --rig-name %H --no-tui
Restart=always
RestartSec=10

[Install]
WantedBy=multi-user.target
```

Then `sudo systemctl enable --now pickaxe-rig`, and `journalctl -u pickaxe-rig -f`
shows its lines. The coordinator runs the same way with its own command.

Windows, a task that starts the rig at logon:

```text
schtasks /Create /TN "Pickaxe rig" /SC ONLOGON /TR "\"C:\Pickaxe\pickaxe.exe\" mine --coordinator 10.0.0.2:3340 --coordinator-key KEY --no-tui"
```

## Network

- Open TCP port 3340 on the coordinator to the rigs' network only.
- Any machine that reaches the port can connect as a rig, but it can only mine
  for your payout, and every winner is checked before it is claimed.
- Up to 1,024 rigs can be connected at once.

### Rigs in other places

- Easiest: install [Tailscale](https://tailscale.com) on the coordinator and
  on every rig, signed in to the same account. Rigs then reach the coordinator
  at its Tailscale address wherever they are, with no router port opened, and
  Connection info shows that address. [Headscale](https://github.com/juanfont/headscale)
  runs the same network on your own server; any VPN that puts the machines on
  one network works too.
- Without a VPN, forward TCP 3340 on the coordinator's router to it and give
  rigs your public IP address or domain; a public pool publishes that address.
- Pickaxe never opens router ports by itself, and the link to every rig is
  encrypted and pinned to the coordinator's key either way.

## Tested

Live on Chipnet on 2026-10-08, with PR #32's build: a coordinator with
`--rigs-only` and two rigs on one PC, rig A on an RTX 5070 Ti Laptop GPU
(CUDA) and rig B on the same PC's AMD Radeon integrated GPU (wgpu), each
process on its own GPU. In 10 minutes the rigs sent 357 winners (rig A 347,
rig B 10). The coordinator checked and claimed 349 of them, each followed by
the next job on both rigs; 3 were stale (found after their job had moved on),
none was rejected, and the last few were not claimed before the test stopped.
A Chipnet Fulcrum server then had 346 of the 349 claims in two blocks and the
other 3 in its mempool. Neither rig reconnected, and the coordinator's process
showed no GPU activity. That build reported each rig's rate as 0; rigs now
measure their own rate (fixed after the test, host-tested).

A public GPU pool, live on Chipnet on 2026-10-08 with PR #40's build: the
coordinator with `--rigs-public --rigs-fee 20` (20% only so fee windows show
in a 10-minute test) and the same two GPUs as two rigs, each with its own
payout: rig A joined at the coordinator's local-network address and rig B at
its Tailscale address, both from the coordinator's `connect` list. The rigs
sent 95 winners (rig A 94, rig B 1) and the coordinator claimed all 95. On
Chipnet, by the output carrying the tokens, 75 claims paid rig A's address,
1 paid rig B's, 16 the operator's fee address (17% of the rigs' claims,
against a 20% fee window) and 3 the donation; none paid anyone else, and all
95 confirmed. Chipnet's PHOTON target was about six times harder than in the
first test.

Not yet tested live: rigs on separate machines, a backup coordinator taking
over, and more than two rigs.
