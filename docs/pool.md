# Running a public pool

Pickaxe's ASIC server can run as a public pool. Miners connect with their own
payout address as the username, and a block they find pays them directly in
its coinbase: there is no pool wallet, no custody and no payouts to send. You
earn a fee that you choose, after the Pickaxe donation. A GPU pool works the
same way for GPU rigs; see [A public GPU pool](#a-public-gpu-pool).

## Start the pool

The pool builds blocks from your own BCH node, as solo mining does (see
[stratum-v2.md](stratum-v2.md)). In the setup choose **Run a pool**, then
**ASIC pool**, and set the pool fee, where it comes from and its address on the
settings page; or from a terminal:

```text
pickaxe_miner stratum-v2 serve --config pool.json \
  --listen 0.0.0.0:3336 --sv1-listen 0.0.0.0:3333 \
  --public --pool-fee 2 --pool-fee-mode coinbase --pool-fee-address <q or p address>
```

Add `--chipnet` to test on Chipnet first. Open ports 3333 (SV1) and 3336 (SV2)
to the internet, through your router's port forwarding or on a server. SV1
is not encrypted; SV2 is, and miners pin the authority key the server prints
when it starts.

## How miners connect

Press `i` on the dashboard for **Connection info**: every address miners use,
for your network and (with Tailscale running) your Tailscale address, with a
number key to copy each line, and what to put as username and password. The
JSON start line (`--no-tui`) lists the same addresses under `connect`.

- SV1 ASICs (most home miners, and every stock Antminer, Avalon and
  Whatsminer firmware, the Antminer S19 and the Avalon Nano 3 included): pool
  `stratum+tcp://<your host>:3333`, username their payout address
  (`bitcoincash:q…` or `p…`, the prefix may be left out), optionally followed
  by `.worker`, any password. Pickaxe translates SV1 for them, so only the hop
  between the device and the pool is SV1.
- SV2 firmware, such as Braiins OS (which runs on Antminers) and Bitaxe:
  `stratum2+tcp://<your host>:3336/<authority key>`, the one-line form ckpool
  and Braiins publish, with their payout address as the user identity.
  Another Pickaxe joins with that same line (Join a pool).
- A username that is not a payout address on the pool's network is refused
  when the device authorizes, with that reason.
- The workers page names each device by its worker name (`rig1` in
  `bitcoincash:q….rig1`), never by its address; a device that gives only an
  address keeps a generated label.

## Miners in other places

- On your network, the addresses on Connection info work as shown.
- **Tailscale** is the easy way to reach a pool from elsewhere without opening
  router ports: computers signed in to the same Tailscale account see each
  other at their Tailscale addresses, which Connection info shows.
  [Headscale](https://github.com/juanfont/headscale) runs the same network on
  your own server.
- ASICs cannot run Tailscale themselves. At their site, run Pickaxe on any
  computer with **Join a pool**, pasting this pool's SV2 line, and point the
  ASICs at that computer: their SV1 stays on that site's network and only
  encrypted SV2 crosses the internet.
- A public pool for anyone forwards its ports and publishes
  `stratum+tcp://<your domain or public IP>:3333`. Pickaxe never opens router
  ports by itself.

## Fees

- **Order**: the Pickaxe donation comes off first, in full (1.5% by default;
  Advanced settings or `--donation` can set it from 0% to 100%). Your pool fee
  comes off what is left, and the miner keeps the rest.
- **Where the fee comes from** (`--pool-fee-mode`):
  - `coinbase`: an output to your fee address in every block a miner finds;
  - `work`: that share of each miner's mining time mines to your fee address,
    so a block found then pays you in full, with no money passing through
    anyone;
  - `both`: one third work and two thirds coinbase, the donation's own split.
- **Fee address** (`--pool-fee-address`): a `q` (P2PKH) or `p` (P2SH, such as
  a multisig, with a 20-byte or 32-byte hash) address; by default your payout
  address.

For example, with the default 1.5% donation and a 2% coinbase fee, a block
paying 3.125 BCH pays the donation 0.03125 BCH (its coinbase part; its work
part is separate mining time), your fee address 0.061875 BCH, and the miner
3.031875 BCH.

The dashboard header shows `Public pool, fee 2.00% from coinbase`. With SV2,
miners see every coinbase output in the jobs they receive, so nobody can hide
a fee; with SV1 they can decode the coinbase parts of each job.

## A public GPU pool

In the setup choose **Run a pool**, then **GPU pool**: this computer
coordinates other people's rigs on port 3340, with no GPU of its own. Each rig
mines for its own address and its wins are claimed to it. A token claim pays
one address, so your fee is a share of each rig's mining time, after the
donation, paid to a `q` address. Connection info (`I`) shows the command a rig
runs to join, with its own address in place of `YOUR_BCH_ADDRESS`. From a
terminal, and the details, see [farm.md](farm.md#a-public-gpu-pool).

## Not yet

- Shared rewards, where every miner gets part of every block: that is P2Pool
  v2 (coming), also without custody.
- Tested end to end with simulated SV1 and SV2 devices on a test node, not
  yet with real public miners.
