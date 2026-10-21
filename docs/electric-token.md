# ELECTRIC, as a second token

Status: `pickaxe electric` mines the published mainnet contract as it is,
with a dedicated fee-pot key managed by Pickaxe. PR #65 signs both claims
and refunds, allowing fresh same-height proofs without the former keyless
pot's height gate. Since PR #58 it searches on GPUs (CUDA, and the portable engine
for integrated and other GPUs), is a token choice in the setup, and has
Pickaxe's own Chipnet deployment, where it has claimed live (below). No
payout-wallet key is needed. The design note of 2026-10-13 (PR #50) is
revised below where the implementation or the chain changed it.

Sources, read 2026-10-13 and 2026-10-15:

- Contract: https://gitlab.com/electricdude/electrictoken/-/blob/main/electric-deployer-miner/contracts/PoWMineStateful.cash
- Compiled artifact (cashc 0.13.3): `electric-deployer-miner/artifacts/PoWMineStateful.json`
- Author miner: `electric-cpp-hybrid-miner/submit.ts` and `electric-webgpu-miner/src/config.ts` in that repository
- Web miner: https://electricminer.netlify.app/
- Category (mainnet): `821f86ed21c38043500d7364fa73108cce8990baf95c4a0925fc44abfc27f8c7`
- Covenant (mainnet, P2SH32): `aa20f3b3cc8e800544dacb055235a1d45d7b7d0804a894935d301ef4796a2b08e70587`, Electrum scripthash `77f4978548da75ea5aa1efff64082f66c62ae0416483b4198bcef515bd1dc4b6`
- Author's own warning: the contract is unaudited and the token may be worthless

## Decision

Mine the published contract beside BCH and PHOTON, on a GPU search, without
the payout wallet's key in Pickaxe. A separate pot-only key protects claims
and refunds. Document its replay weakness, never use it, and keep the
deployment as data so a fixed contract is one new `Contract` variant.

## Replay

The proof is `hash256(lastHeight || recipientPkh || nonce)`. It does not
cover the baton's outpoint or `mineCount`. A claim whose `tx.locktime`
equals `lastHeight` leaves `lastHeight` where it was, so the same proof
passes again on the next baton at that height, at difficulty 4 for a proof
with 4 zero bytes. A claimer who keeps the locktime at `lastHeight` keeps
the height from ever moving, and one proof pays out for as long as they
pay relay fees.

This is happening. Every claim on mainnet, read through Fulcrum on
2026-10-15 at tip 972985:

| Measure | Value |
|---|---|
| Claims | 1,412 |
| Distinct (recipient, nonce) proofs | 302 |
| Claims that reused an earlier proof | 1,109 (79%) |
| Recipients | 8; 7 of them reused proofs |
| Most uses of one proof | 137 |
| Block 972328 | 135 claims, 131 from one proof, locktime 972327 |

The last claim was at height 972909. Rewards left: 599,915,280,000,000 units
of 600,000,000,000,000.

A fix is the author's to deploy. Covering the baton's state in the proof
(for example `mineCount`, or the outpoint being spent) or requiring
`tx.locktime > lastHeight` closes it. The operator told the author on
2026-10-15; the author said he will improve the contract when he has time
and will not create another token. Until then the published contract is
the one mined.

What Pickaxe does about it:

- It never sends a proof twice (a set of sent proofs per run), and every
  search starts from fresh work on the current baton.
- Older restricted pots pay only for a claim that moves the baton to a new height, or
  for the claim right after it at the minimum difficulty. Those are the two
  claims per height that cannot be a replay of an earlier claim on that
  height. A later claim on the same height is the shape replays take, and
  the pot refuses it even when the contract would accept it.
- While a replayer is active, Pickaxe loses most races: a replay costs no
  work. That is a loss of income, not of funds; a lost race leaves the pot
  unspent.

## What the contract demands

`mine(recipientPkh, nonce)` on input 0, which must be the mutable NFT:

- Proof: `hash256(lastHeight || recipientPkh || nonce)` has `difficulty`
  leading zero bytes. `lastHeight` is 4 little-endian bytes. Pickaxe uses an
  8-byte nonce, as the author's miner does, so the preimage is one 32-byte
  SHA-256 block and each try is two compressions.
- `difficulty`, `lastHeight` and `mineCount` live in a 16-byte commitment.
  The published deployment uses `MIN_DIFF = 3`, `MAX_DIFF = 4` and
  `REWARD = 60_000_000` base units per claim.
- If `tx.locktime` is above `lastHeight`, the next difficulty is `MIN_DIFF`
  and `lastHeight` becomes the locktime. Otherwise it rises by one, capped at
  `MAX_DIFF`. The proof is checked against the stored difficulty, so the
  cheap claim is the one right after a claim that moved the height.
- The contract has cashc's locktime guard: input 0 needs a non-final
  sequence (Pickaxe uses `0xfffffffe`, as the claims on chain do).
- Output 0 is a P2PKH of `recipientPkh` with the entire reward and exactly
  1,000 sats. Output 1 continues the covenant with at least 1,000 sats. An
  optional output 2 carries no token. A fourth output fails.

Two claims accepted at height 972909 are test vectors in `src/electric.rs`:
`c5359e4d…e028` moved the baton from height 972844 to 972908 with a 4-zero
proof, and `835dfb0c…ee18` was the cheap claim after it with a 3-zero proof.

## ASICs, and why this is not merge mining

The algorithm is SHA-256d, so a SHA-256 engine can grind it, but a BCH ASIC
hashes an 80-byte header. ELECTRIC hashes 32 bytes with the recipient right
after the height. Stratum cannot aim the nonce field at this preimage, and
[merge-mining.md](merge-mining.md) Case A and [asic-tokens.md](asic-tokens.md)
both need a contract that accepts header work. This one does not, so the
ASIC farm keeps mining BCH and GPUs search ELECTRIC beside it.

PHOTON cannot share work with it either; on one GPU the two split the time.
A new ELECTRIC contract could be merge-mined with PHOTON as the parent,
without changing PHOTON: see [photon-electric-auxpow.md](photon-electric-auxpow.md).

## Signed fee-pot claims

### Pickaxe-controlled receiving wallet and owner pot

#### PR #65

`pickaxe electric wallet` creates or reopens a dedicated payout key beside
the configuration, separately for each network. It prints the public receiving
address and QR, plus the private backup file's path. The key itself is never
printed. Back it up before using the address. The existing payout wallet,
profile and pot keys are not imported or overwritten. This is a separate
receiving wallet, not a recovery key for previous payouts.

To prepare a new owner-controlled fee pot for the selected receiving address:

```text
pickaxe --address YOUR_RECEIVING_ADDRESS electric pot --owner-pot
pickaxe --address YOUR_RECEIVING_ADDRESS electric mine --owner-pot
pickaxe --address YOUR_RECEIVING_ADDRESS split --share photon=80 --share electric=20 --owner-electric-pot
```

These flags are explicit: existing startup commands retain the previous pot.
The owner pot uses its own key file and funding address. Its claims remain
signed and keep the same recipient, change and fee constraints. Its refund
branch allows a chosen destination only with the committed owner's signature;
it still permits exactly one input and one plain-BCH output with a capped fee.
It does not redirect funds held by an older covenant. Those funds can refund
only to the destination permanently committed by that older covenant, then
need that wallet's spending key to move again.

In the owner-pot TUI, **F** opens funding, **M** selects Max/send. Enter the
destination, press **Enter** to preview the total amount, fee and transaction
count, then **Y** to send after the quote is displayed. Holding or repeating
Enter cannot confirm a transfer. **Esc** cancels an unapproved quote.
Addresses are checked against the session's network. A quote expires after
60 seconds and is invalidated if coins change or the relay fee rises. Max
sends each pot coin separately, each within the covenant fee cap; an
uneconomic coin prevents the whole quote rather than silently burning it.
This transfers plain BCH from the pot, not token rewards from the payout wallet.

The supervisor finishes any pending reward claim before quoting, then holds
ELECTRIC's pot while editing or sending; PHOTON can continue. Exact approved
transfers are saved privately before broadcast, restored after restart and
counted as propagated only after fetching identical transaction bytes. A
timeout or conflicting spend retains the journal and keeps its inputs reserved.
Approved transfers cannot be cancelled by closing the panel. The same pot's
claim and refund commands take an OS lock. New owner pots share one journal
location per OS account and pot identity, even across different config folders;
older pots retain their existing journal locations. Files are retained for recovery.
Keep the original key and journal backups until all transfers are settled.

The receiving-wallet key and pot key are separate. A newly created empty pot
must be funded before mining can use it; creating keys moves no existing BCH.

#### PR #65

New pots require the dedicated pot key to sign each claim and refund.
Fresh proofs can be mined repeatedly at the same block height, up to the
token contract's difficulty limit. The old height gate is unnecessary for
these pots: a public ELECTRIC proof alone cannot authorize spending their BCH.
The signature commits to both input outpoints, the spent pot amount and all
outputs. The covenant still restricts the reward recipients, exact transaction
shape, change to the same pot and the maximum fee. Pending signed claims are
saved before broadcast and rebuilt with the same key on recovery.

**Existing funded pots:** their scripts and addresses cannot change in place.
The same recovery-key file is reused; it is never replaced. `electric status
--restricted-pot` and `electric refund --restricted-pot` select the previous
owner-recoverable pot. Refunds still return BCH only to the original payout
wallet. Use that wallet to fund the new signed pot shown in setup or by
`electric pot`. Review the refund transaction preview before
adding `--broadcast`. Pickaxe never moves these funds automatically or imports
the payout wallet's key. `--legacy-pot` continues to select the oldest pot
without a recovery key. Each version keeps its own pending journal.

The following historical keyless-claim description applies only to those
older funded pots. Their protection is retained for recovery and compatibility.

Live Chipnet check (2026-10-21): a separately funded signed pot completed four
claims at height 328750. Start9 BCHN independently returned all four exact
transactions; three were same-height continuations, two above the minimum
difficulty. They paid 938 sats each for 938 bytes (3,752 sats total fees),
and delivered 4,000 sats alongside their token rewards. The miner then waited
for more pot funding, with no pending claim. Mainnet pot funds were untouched.
The independent standard/consensus BCH VM suite covers 33 valid and refused
claims/refunds, including wrong-key/unsigned claims, changed outpoints and
amounts, duplicate pots, and all permitted recipients.

## Previous owner-recoverable pots

#### PR #51

Pickaxe creates a dedicated recovery key for the fee pot. It does not import
or store the payout wallet's key. Claims remain keyless; only refunds need
the recovery signature. PHOTON and its runtime keys are unchanged.

`pickaxe electric pot --address <payout>` creates the key before displaying
the funding address, or reuses the existing key. #### PR #53: it prints the
address's QR code too, the same one the setup shows. Fund that P2SH32 address
with plain BCH from any wallet. The default key file is beside the selected
`--config`, named for the network and the pot's contract/payout parameters.
`--pot-key <path>` selects a backed-up key explicitly; use it consistently
on `pot`, `status`, `mine` and `refund`.

**Back up the key file before funding.** It contains a raw 32-byte secret,
protected by owner-only file permissions (Windows ACLs or Unix 0600), not
password encryption. Pickaxe never prints its contents. Someone running as
your OS account can read it; protect backups too. Losing the key prevents
refunds. Damaged files are refused, never overwritten. `status`, `mine` and
`refund` refuse a missing key rather than silently creating a different pot.
`pot` is the explicit creation command: if you deleted the old key, restore
its backup instead of creating a new pot for already-funded coins.

The script commits the ELECTRIC covenant, minimum difficulty, payout,
donation addresses, fee cap and recovery public key. It permits two spends:

- **Claim** (input 1 of exactly 2): input 0 is the ELECTRIC covenant;
  output 0 pays the payout or a donation address, including 1,000 sats;
  output 1 keeps the baton's sats exactly; output 2 returns plain-BCH
  change to the same pot. The fee is capped at 3,000 sats. The claim must
  move the baton to a new height or be the minimum-difficulty claim after
  it. No key or signature is needed for this branch.
- **Refund** (the pot alone): the committed recovery key must sign the
  entire transaction. Exactly one output returns BCH to the configured
  payout, with no token and a fee at most 3,000 sats. `electric refund`
  prints the signed transactions; `--broadcast` sends them. If the relay
  fee exceeds the cap, Pickaxe refuses rather than underpaying it.

The recovery key cannot redirect a refund or change donation recipients.
Mining loads it only to derive the public key; the secret is dropped before
search starts. This is separate from the ephemeral PHOTON search keys.

### Existing keyless pots

A recovery key changes the pot address; it cannot retroactively protect an
already-funded covenant. `--legacy-pot` selects the original keyless script
and its original claim/refund behavior, without creating a key file. Use
`electric refund --legacy-pot` with the original payout/deployment settings
to recover those coins, then fund the new address. Do not send funds to an
old address expecting the new recovery protection.

Anyone can still construct a valid claim, but its reward must pay one of
the committed recipients. They cannot force a refund of a new pot without
the recovery key. Keep the pot small: each claim spends 1,000 sats on the
reward output plus its relay fee. A valid donation claim pays that recipient,
not the miner. The published contract's replay weakness remains unchanged.

When the pot cannot pay a claim, the miner does not search. A proof found
while the pot ran dry is held and sent after a top-up if the baton has not
moved; otherwise it is dropped.

## Donation

The same as PHOTON: 4% of the search work, decided before each batch is
hashed. On mainnet 2% for `DONATION_ADDRESS` and 2% for
`SHREC_DONATION_ADDRESS` (both P2PKH; `z` is token-aware P2PKH); on Chipnet
4% for `CHIPNET_DONATION_ADDRESS`. A batch searched for a donation address
puts that address in the proof, so its win pays it the whole reward and
nothing is split later. `--token-donation` above 4% adds work for the
project address only.

The hybrid in PR #50's note (2% work and 2% of each personal reward) is not
used. The contract forbids a second token output, so the reward split would
have to be a second transaction from the payout wallet, which needs a key or
a wallet session, and personal mining would stop whenever that signer is
away. The work share needs neither.

## Racing

The proof is cheap: 2^24 hashes at difficulty 3, 2^32 at 4. #### PR #58:
Pickaxe searches on the selected GPUs: CUDA through its own module
(`cuda/build/electric_rust.ptx`) and every other GPU through the portable
engine (`reference/shared-electric`), both generated from the shared Rust
proof in `rust-engine/src/electric.rs`. ELECTRIC's CUDA module is built
separately from PHOTON's (`tools/build-rust-kernels.*`), so PHOTON's
compiled kernels do not change: PHOTON's PTX built from this branch's
sources is byte-identical to dev's (`243beaec…`, 2026-10-18). The host checks every returned nonce before a claim
is built, and the donation share is assigned before each batch is hashed, as
on the CPU before. (CPU-only results from 2026-10-15: 268 to 285 MH/s on 31
desktop threads.) Web miners race for the same baton, so expect to win
mostly when they are idle. `pickaxe electric mine --dry-run` searches
without a funded pot and sends nothing, to show a computer's rate first.
`--max-difficulty 3` searches only the cheap claims.

## Mining from the setup

#### PR #58: in the setup, Token lists ELECTRIC after the GPU tokens. Its
settings are the GPUs, the payout address, intensity and, under Advanced,
the donation. As soon as the payout address is in, the setup makes the fee
pot and its recovery key (beside the configuration, owner-only) and shows
the pot's address, a QR code of it and where the key is saved, so the miner
can fund the pot from a wallet before starting. Start then opens the
ELECTRIC screen: rate, accepted rewards, the pot's balance and how many
claims it pays, and the QR code again whenever the pot cannot pay a claim
(`f` shows or hides it). `--no-tui` prints the same address, QR code and
key path once, then each state and event as a line.

#### PR #53: Start first reads the pot's balance. A pot that cannot pay a
claim yet (or cannot be read) is reported, and the next Start press starts
anyway. To mine ELECTRIC beside PHOTON on the same GPUs, each for a share of
the time, choose Hash split instead ([hash-split.md](hash-split.md)).

The ELECTRIC choice saves no profile. Profiles reject unknown fields, and
an older Pickaxe reading the same file must keep working; pick ELECTRIC in
the setup each time.

## Chipnet

There is no published Chipnet deployment, so Pickaxe made one (PR #58),
and `--chipnet` mines it with no other flag: category
`39629630f3f5ba91c6e656e7b79c2c8de2fc6f776bfe085e1b7eaf70aa8a5e76`
(genesis `6ba9682c719248f473ec6b3eb5efe3650b55439b751463ea1dec65c906163ee1`,
2026-10-18), with the mainnet constructor values (60,000,000 units a claim,
difficulty 3 to 4) and one million claims on the baton. Another deployment
still takes `--electric-category` and its values.

`pickaxe electric deploy --funding TXID:0 --funding-sats N` builds such a
genesis: the coin at output 0 of its transaction becomes the category, the
baton starts at the minimum difficulty and the chain tip with the whole
supply, and the change goes to `--change` or `--address`. It prints the
unsigned transaction; the funding coin's wallet signs and sends it, so
Pickaxe never holds that key. The Chipnet genesis was funded with 10,000
sats from a public Chipnet faucet and signed by the node wallet holding
them.

Live check, 2026-10-18, on the operator's PC (RTX 5070 Ti Laptop GPU over
CUDA and the AMD integrated GPU over the portable engine, `--device 0,1`),
with the PC's PHOTON miner stopped for the window and restarted after it:
the pot was funded with 8,000 sats from a wallet, and the session found two
proofs and claimed both. The first moved the baton to height 328452 at
difficulty 3, and the second, the cheap claim after it, raised it to 4;
each paid 60,000,000 units and 1,000 sats to the payout, and
took 1,851 sats (1,000 plus an 851-sat fee) from the pot, which kept 4,298.
Both confirmed in block 328453, and two public Chipnet Fulcrum servers
agree on their outputs and the baton chain from the genesis. The pot pays
two claims a block, and no block came in the rest of the window, so the
session waited as designed. Still to show live: a lost race that leaves the pot
unspent, a held proof sent after a top-up, and a signed refund.

## When the contract is fixed

The deployment is data (`electric::Deployment`): category, reward,
difficulties and a `Contract` variant that names the compiled body and the
proof's preimage. A fixed contract is a new variant, selected by its
category. Its locking bytecode differs, so its pot is a new address; refund
the old pot to the payout first.

## Checks

- `cargo test --lib electric`: the mainnet covenant computed from the body
  and constructor values, proofs and state changes of the two claims above,
  the search against plain hashing, the donation schedule in the parallel
  search, and claim and refund sizes and fees.
- `tools/reward-policy-vm/electric.mjs` (in `npm test`): rebuilds the
  covenant from the body in JavaScript and checks it against the covenant
  holding the mainnet baton, then runs the Rust-built claims and refunds
  through the BCH 2026 VM in standard and consensus modes. Claims to the
  payout and both donation addresses, the cheap same-height claim and the
  refund pass; a same-height claim above the minimum, a claim to another
  recipient, a fee above the cap, a refund elsewhere, missing or attacker
  recovery signatures, altered signed transactions and duplicate pot inputs
  fail. Rejected claim vectors pass the ELECTRIC input first, then fail at
  the pot's input.
- `pickaxe electric status` read the live baton through Fulcrum on
  2026-10-15 (claim 1412, difficulty 4, height 972908).
- #### PR #58: `cargo test --lib electric` also checks the genesis builder
  (output 0 only, the baton's state and supply, the fee for the signed
  size), the Chipnet deployment's covenant, the fee pot's QR code module
  for module, and the ELECTRIC screen showing an unfunded pot's QR code;
  `tui::tests::electric_setup_shows_the_fee_pot_to_fund_and_saves_no_profile`
  walks the setup. The ignored GPU tests
  (`electric::gpu::tests`, `--ignored --test-threads=1`, miners stopped)
  compare every hash and the first winning nonce with the CPU across nonce
  boundaries; they passed on CUDA (RTX 5070 Ti) and on the portable engine
  with the AMD integrated GPU and the NVIDIA GPU on 2026-10-18.

## Not in this change

- Not inside `stratum-v2 serve`; run `pickaxe electric mine` beside it.
- No `MiningToken` variant: PHOTON's runtime matches on that enum; the
  setup lists ELECTRIC beside it and starts ELECTRIC's own session.
- No saved ELECTRIC profile (see "Mining from the setup").
- Refunds are signed locally and broadcast only with `--broadcast`.
