# Running a public BCH pool

Pickaxe's ASIC server can run as a public pool. Miners connect with their own
payout address as the username, and a block they find pays them directly in
its coinbase: there is no pool wallet, no custody and no payouts to send. You
earn a fee that you choose, after the Pickaxe donation.

## Start the pool

The pool builds blocks from your own BCH node, as solo mining does (see
[stratum-v2.md](stratum-v2.md)). Then:

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

- SV1 ASICs (most home miners): pool `stratum+tcp://<your host>:3333`,
  username their payout address (`bitcoincash:q…` or `p…`, the prefix may be
  left out), optionally followed by `.worker`, any password.
- SV2 devices, such as Bitaxe: `<your host>:3336` with your authority key,
  their payout address as the user identity.
- A username that is not a payout address on the pool's network is refused
  when the device authorizes, with that reason.

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

## Not yet

- Shared rewards, where every miner gets part of every block: that is P2Pool
  v2 (coming), also without custody.
- The workers page shows generated labels, not miners' addresses.
- Tested end to end with simulated SV1 and SV2 devices on a test node, not
  yet with real public miners.
