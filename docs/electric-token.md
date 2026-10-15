# ELECTRIC, as a second token

Status: `pickaxe electric` mines the published mainnet contract as it is, on
the CPU, with the relay fee paid from a keyless pot (PR #51). No key, no WIF,
no contract deploy. Nothing has been broadcast by Pickaxe yet: the first
funded run is the operator's. The design note of 2026-10-13 (PR #50) is
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

Mine the published contract beside BCH and PHOTON, on a CPU search, without
a key in Pickaxe. Document its replay weakness, never use it, and keep the
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
- The pot pays only for a claim that moves the baton to a new height, or
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
ASIC farm keeps mining BCH and the CPU searches ELECTRIC beside it.

## Paying the fee without a key

The fee needs BCH that is not on the baton. The options:

| Option | Why not, or why |
|---|---|
| A throwaway WIF in the miner | The key can sign any transaction, not just a claim. Refused by the operator. |
| The baton's own sats | The covenant keeps 1,800 and allows 1,000; 800 sats pay one fee, once, and leave every later claimer worse off. |
| The wallet signs each claim (a WalletConnect-style session) | The wallet has to be online and answer within a second or two of a win; the race is lost in that time. |
| Inputs pre-signed with ANYONECANPAY | Wallets do not offer it, and anyone who sees such an input in our claim can spend it in theirs. |
| A keyless pot covenant | Chosen. Funded once from any wallet; it can only pay our claims or go back to the payout. |

### The pot

`pickaxe electric pot --address <payout>` prints its P2SH32 address. The
script commits the ELECTRIC covenant's locking bytecode, its minimum
difficulty, the payout and both donation addresses, and the fee cap, so the
address changes with any of them. Two spends:

- **Claim** (the pot is input 1 of 2): input 0 is the ELECTRIC covenant;
  output 0 pays the payout or a donation address (the contract makes it the
  reward, 1,000 sats); output 1 keeps the baton's sats exactly; output 2
  returns the rest to the pot with no token; the fee is at most 3,000 sats;
  and the claim moves the baton to a new height or is the minimum-difficulty
  claim after it.
- **Refund** (the pot alone): one output, to the payout, taking at most
  3,000 sats as fee. `pickaxe electric refund` prints these transactions and
  sends them with `--broadcast`.

Anyone can spend the pot, but only into one of those shapes. Someone who
uses it to pay for a claim sends the reward to us. The worst a stranger can
do is spend fees on our behalf: at most 3,000 sats a claim and two claims a
height, each paying us 60 ELECTRIC and returning the 1,000 sats with them.
Keep the pot small (a few thousand sats a claim; a claim is about 800 bytes,
800 sats at the 1 sat/byte relay fee).

When the pot cannot pay a claim, the miner does not search. A proof found
while the pot ran dry is held and sent after a top-up if the baton has not
moved; if it has, the proof is dropped. That win was never ours.

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

The proof is cheap: 2^24 hashes at difficulty 3, 2^32 at 4. Pickaxe searches
on the CPU (`--threads`, default all but one) with the hardware SHA
instructions where the CPU has them. On a 32-thread desktop CPU, 31 threads
searched the live baton at 268 to 285 MH/s in a dry run on 2026-10-15: a
difficulty-4 proof in about 15 seconds on average, a difficulty-3 proof in
well under one. GPU web miners can be faster still, so expect to win mostly
when they are idle. `pickaxe electric mine --dry-run`
searches without a funded pot and sends nothing, to show a computer's rate
first. `--max-difficulty 3` searches only the cheap claims.

## Chipnet

There is no published Chipnet deployment. Deploy the same contract with a
wallet you already have, then pass `--chipnet --electric-category <id>` (and
`--electric-reward`, `--electric-min-difficulty`, `--electric-max-difficulty`
if they differ). Pickaxe will not generate the deploy key. On that
deployment the CPU search, the pot, a won claim, a lost race that leaves the
pot unspent, a held proof sent after a top-up, and a refund can be checked
before mainnet.

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
  recipient, a fee above the cap and a refund elsewhere fail at the pot's
  input after the contract's input passed.
- `pickaxe electric status` read the live baton through Fulcrum on
  2026-10-15 (claim 1412, difficulty 4, height 972908).

## Not in this change

- No GPU search; the CPU only.
- No setup-wizard screen; `pickaxe electric pot` prints the address.
- Not inside `stratum-v2 serve`; run `pickaxe electric mine` beside it.
- No `MiningToken` variant: that enum is the GPU tokens.
- No broadcast, no deploy, no key.
