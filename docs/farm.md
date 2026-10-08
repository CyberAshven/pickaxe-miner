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
the farm's rate, winners and rejected winners, the listen address and the key)
and one row per rig (name, GPUs, rate, winners, minutes connected). Its
Hashrate row adds the rigs' rate to the coordinator's own. `--json` status
lines carry the same under `rigs`.

## Rigs

```text
pickaxe mine --coordinator <coordinator address>:3340 --coordinator-key <key> --rig-name rack1-07 --no-tui
```

A rig needs no address, Fulcrum server or node of its own. It mines on every
discrete GPU (`--device` and `--include-integrated` choose others), and prints
one line per event: its GPUs at start, `connected`, each `job` and `winner`,
and a `status` line with its rate every 10 seconds; `--json` prints them as
JSON. Repeat `--coordinator` and `--coordinator-key`, in the same order, for
backup coordinators. The name defaults to the computer's name.

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

Not yet tested live: rigs on separate machines, a backup coordinator taking
over, and more than two rigs.
