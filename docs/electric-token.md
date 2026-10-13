# ELECTRIC, as a second token

Status: design only. No miner, no contract deploy, and no key. Written
2026-10-13 from the published `PoWMineStateful` source, the author's miner,
and Pickaxe's PHOTON fee path. ELECTRIC stays off until a later change
selects it in `MiningToken`.

Sources, read 2026-10-13:

- Contract: https://gitlab.com/electricdude/electrictoken/-/blob/main/electric-deployer-miner/contracts/PoWMineStateful.cash
- Author miner: `electric-cpp-hybrid-miner/submit.ts` and `electric-webgpu-miner/src/config.ts` in that repository
- Web miner: https://electricminer.netlify.app/
- Category (mainnet): `821f86ed21c38043500d7364fa73108cce8990baf95c4a0925fc44abfc27f8c7`
- Author's own warning: the contract is unaudited and the token may be worthless

## Decision

Mine it beside BCH, on a CPU search, without a WIF in Pickaxe.

The published contract cannot do what PHOTON does. PHOTON's baton holds the
relay fee, and each claim draws a bounded amount from that baton, so the
miner never loads a key. ELECTRIC's `mine` keeps the covenant near 1,800
sats and requires an extra input for the fee. There is no function that
adds sats back. Taking the covenant down to its 1,000-sat floor would
pull 800 sats out once and leave every later claim worse off. This design
does not do that.

A throwaway WIF is the wrong fix. The key can pay any transaction, not
only a winning claim, and it sits in the miner. A lost race does not need
a key at all: two claims spend the same baton, the loser never confirms,
and its fee input stays unspent. The fee leaves only when our claim is
the one in a block.

The replacement is a pot covenant the miner funds from a normal wallet.
Pickaxe compiles the script, shows the cashaddr in the setup wizard, and
watches for the payment. It never asks for a WIF and it never generates
a key. The pot has two spends: a claim fee, or a refund to the payout
address baked into the script.

## What the contract demands

`mine(recipientPkh, nonce)` on input 0, which must be the mutable NFT:

- Proof: `hash256(lastHeight || recipientPkh || nonce)` has `difficulty`
  leading zero bytes. The author's miner uses an 8-byte nonce, so the
  preimage is 32 bytes. `hash256` is SHA-256d.
- `difficulty`, `lastHeight` and `mineCount` live in a 16-byte commitment.
  Constructor bounds in the published miner are `MIN_DIFF = 3` and
  `MAX_DIFF = 4`. The web miner pays 60 ELECTRIC per win
  (`REWARD = 60_000_000` base units) from a supply of 600,000,000.
- If `tx.locktime` is above `lastHeight`, the next difficulty is
  `MIN_DIFF`. Otherwise it rises by one, capped at `MAX_DIFF`. Many wins
  fit in one BCH block, and the first win after a new height is the easy
  one.
- The hash commits to the baton's `lastHeight` and the recipient, not to
  the new block. A saved nonce stays valid across later BCH blocks until
  somebody spends that baton.
- Output 0 is a P2PKH of `recipientPkh`, the entire reward, and exactly
  1,000 sats. Output 1 continues the covenant. An optional output 2 is
  pure BCH. A fourth output, or a second token output, fails.

Three or four leading zero bytes is 24 or 32 bits (`2^24` or `2^32`
hashes). That is a CPU search. It is also why a win lands long before
the next BCH block: the author's miner then broadcasts a fee-paying
transaction, and any BCH miner can include it.

## ASICs, and why this is not merge mining

The algorithm is SHA-256d, so a SHA-256 engine can grind it. A BCH ASIC
does not. It hashes an 80-byte header. ELECTRIC hashes 32 bytes with the
recipient sitting immediately after the height. Those are not the same
bytes, and Stratum cannot aim the nonce field at this preimage.

[merge-mining.md](merge-mining.md) Case A needs a token that accepts the
header hash. [asic-tokens.md](asic-tokens.md) needs an 80-byte commitment
the device hashes instead of BCH. This contract does neither. Pointing a
farm's ASICs at it would take them off BCH for a search one CPU core
finishes in about a second at difficulty 3, and in a few minutes at
difficulty 4.

So the ASIC farm keeps mining BCH. A CPU sidecar searches ELECTRIC for
the payout address already configured on that farm. When the farm is
solo mining, the claim is an ordinary transaction in the template as
well as on the public network. Inclusion in our own block is a bonus.
It is not a way to skip the fee: a 0-fee claim we keep private loses to
the next person who pays the relay fee and gets into someone else's
block. With this difficulty, that happens long before a small farm finds
a BCH block.

## The pot

The author's `submit.ts` adds a P2PKH input signed by `MINER_WIF`, keeps
1,800 sats on the covenant, pays 1,000 sats onto the token output, and
sends 1,800 sats as the fee. Output 2 is the WIF wallet's change. The
same shape works if input 1 is a covenant instead of a key:

- Input 0 is the baton, unlocked by `mine`.
- Input 1 is the pot.
- Output 0 is the allowed recipient (the miner, or a donation address),
  with the full reward and 1,000 sats.
- Output 1 continues the covenant at the same satoshi amount it had.
- Output 2 is the pot's change, locked to the pot again, with no token.
- The fee is inputs minus outputs. The script requires that fee to meet
  the relay floor Pickaxe already tracks for PHOTON, and caps how far
  one claim may drop the pot.

The pot script commits the electric category, the reward amount, the
miner's payout, both donation addresses, and the refund address. A bug
in the miner cannot send the pot to an arbitrary script: any other
shape fails the covenant. Refund is the other path, one output, back to
that refund address.

The 1,000 sats on output 0 are not a fee. They sit on the token output
and come back when those tokens are spent. The only sats that leave for
good are the relay fee of a claim that confirms.

### When the pot runs dry

Stop broadcasting before the pot cannot cover dust plus one fee. Keep
the winning nonce, the recipient, the difficulty and the baton outpoint
in the claim journal. The setup wizard shows the same cashaddr for a
top-up.

On top-up, rebuild and broadcast if that outpoint and commitment are
still the ones we searched. The nonce is still a win. If anyone else
spent the baton while we waited, drop the nonce and say so. That win
was never ours; the tokens are on their output, and our pot was not
spent.

Refund spends the pot's current outpoint back to the payout address.
The wizard refuses a refund while a claim spending that outpoint is
unconfirmed, because the refund would replace our own claim. Confirmed
tokens do not sit in the pot, so a refund does not have to "bring the
tokens with it". They are already on the payout address. Unconfirmed
work is a saved nonce, not a balance, and it is submitted again after
the top-up when the baton is unchanged.

A lost mempool race costs nothing. The competing claim spends the same
baton, ours does not confirm, and the pot input is still ours.

## Donation

PHOTON's mainnet policy is a 4% work share, split in half:

- `bitcoincash:qqn3aqnrarpvecss9vned5v9693j9p37w5pmzz4mn3`
- `bitcoincash:zqqpfwsvht3uaf4y5sm53me90edmtx8cmyd0xx3fv3`

Those are `DONATION_ADDRESS` and `SHREC_DONATION_ADDRESS`. ELECTRIC uses
the same two addresses and the same 4% total, through the hybrid mode
`donation.rs` already defines and PHOTON cannot select (its claim has no
room for a reward split, so selecting one fails before hashing):

- Work: 100 bps to each address. The recipient is fixed before the
  search, as PHOTON's work fee is. A work win pays that address the
  whole reward. No sweep, no key.
- Reward: 100 bps of each personal win to each address. `mine` forbids
  a second token output, so this is the next transaction, spending
  output 0. The wizard's wallet signs that sweep. Pickaxe holds no key
  for it. Personal search does not start until that signer is connected,
  and it pauses if the signer drops. Work-fee search does not need the
  signer.

Hybrid already refuses to take the reward cut from a work win. Expected
donation is therefore `2% + 98% * 2% = 3.96%` of tokens, not a flat 4%.
That is the existing rule ("the reward split applies only to personal-work
wins"). Raising the Advanced donation above this 4% minimum adds work
for the project address only, which is what `fee_policy_at` does for
PHOTON. The reward split stays 2%.

This is an exception to `NEW_TOKEN_DONATION` (1.5% for a token added
after PHOTON). ELECTRIC's minimum is 4%, matching PHOTON's total, with
the hybrid shape above. It is not selected in code by this note.

On Chipnet, PHOTON pays `CHIPNET_DONATION_ADDRESS`
(`bchtest:qrzq5f9ltv70u4su7d40agd4nlnp8qlgqcma6x2tvp`) and does not use
the mainnet collaborator address. ELECTRIC should do the same: both
halves of the work share and both halves of the reward share pay that
one Chipnet address.

The reward sweep is where a wallet session is actually required. Funding
and refund of the pot are plain payments to and from a cashaddr. The
wizard can show that address and watch the chain with no connect
protocol. Signing the 2% split needs a session that returns a signature
and never a WIF. Which wallet speaks that session is open; the first
implementation should fail closed without it rather than skip the split.

## Chipnet

The published web miner is mainnet only. `submit.ts` accepts
`NETWORK=chipnet`, and no Chipnet category is in the published config.
The mainnet category must not be mined on Chipnet.

A Chipnet test is a deploy of this same contract, funded by a wallet
the operator already has. Pickaxe will not generate that key. After the
category exists, the CPU search, the pot, a won claim, a lost race that
leaves the pot unspent, a refill that submits a saved nonce, and a
refund can be checked on Chipnet before mainnet. This note does not
deploy it.

## Not in this change

- No `MiningToken` variant, no search, no pot script, no wizard screen.
- No use of the covenant's own 1,800 sats as a fee.
- No WIF field and no generated key.
- No claim that BCH ASICs merge-mine this contract.
