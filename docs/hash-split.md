# Token hash split

#### PR #53

Status: one session mines several GPU tokens on the same GPUs, each for
its share of the time in 5% steps, at most 100% together. From the setup
(Hash split, after the tokens) or `pickaxe split`. Today the tokens are
PHOTON and ELECTRIC; a GPU token added to `MiningToken::GPU_SUPPORTED` (or a
deployed ELECTRIC on a network) joins the list without a change here.

## Why a split, not shared hashes

PHOTON and ELECTRIC are both SHA-256d, but each contract accepts only the
hash of its own data, so one hash counts for one token
([photon-electric-auxpow.md](photon-electric-auxpow.md)). Running the two
miners side by side splits the GPU anyway, with no say in how: on the RTX
5070 Ti Laptop GPU PHOTON kept 64% of its solo rate and ELECTRIC 35%. On one
GPU their kernels queue behind each other, so each one's measured batch
time also included the other's. The split decides the shares instead and
runs one token's kernels at a time.

## How it works

- One process, one GPU lock and one Ctrl+C handler for every token. Each
  PHOTON-runtime token runs its own supervisor (its own Fulcrum or node
  connection, claim journal and donation schedule) at intensity 100;
  ELECTRIC runs its own session with its fee pot and pending-claim journal.
- A scheduler hands the GPUs to one token at a time in 2-second rounds: a
  5% share is 100 ms of a round. The others are held at their next batch
  boundary (a few milliseconds: a PHOTON batch is about 10 ms on this GPU,
  an ELECTRIC batch well under 1 ms). A held search is not paused: its job,
  claim and winner handling go on.
- A token that cannot mine now gives its time to the others in proportion
  to their shares, and gets it back as soon as it can: ELECTRIC while its
  fee pot cannot pay a claim or its baton is waiting for a block, PHOTON
  while it claims a winner or is paused. Its turn ends early when that
  happens mid-round.
- The shares' remainder to 100% idles. With no token able to mine, the
  round idles and ends as soon as one can.
- #### PR #61: a token that starts or stops being able to mine starts a new
  round at once, so its GPUs and time go to the others without waiting for
  the round to end.
- Each token keeps its own donation: 4% of that token's work for PHOTON and
  ELECTRIC, decided before each of its batches as without a split.

## Several GPUs

#### PR #61

The shares are parts of the whole rig, not of each GPU. Whole GPUs go to
the tokens first; only the GPUs left over take turns:

| GPUs | Shares | Each round |
| --- | --- | --- |
| 1 | PHOTON 80 / ELECTRIC 20 | the GPU takes turns, 1.6 s and 0.4 s |
| 2 | PHOTON 80 / ELECTRIC 20 | GPU 1 PHOTON; GPU 2 takes turns 60/40 |
| 3 | PHOTON 50 / ELECTRIC 50 | GPU 1 PHOTON, GPU 2 ELECTRIC; GPU 3 takes turns 50/50 |
| 5 | PHOTON 80 / ELECTRIC 20 | GPUs 1 to 4 PHOTON, GPU 5 ELECTRIC; no turns |
| 5 | PHOTON 60 / ELECTRIC 20 | GPUs 1 to 3 PHOTON, GPU 4 ELECTRIC, GPU 5 idles |

- Each GPU, the fastest first, goes whole to the token (or the idle rest of
  100%) that needs the most of the rig's speed, if the GPU is not more than
  that; on a near tie the token listed first keeps it. The GPUs left over
  take turns for what each token still needs, as one GPU does.
- The GPUs are weighed by speed, so a fast card and a slow one split by
  hashes, not by count. For the first three rounds (6 s) every GPU takes
  turns, so each token mines every GPU; then each GPU's rate while mining
  (not while held) is read from the token with the largest share that
  measured every GPU, and GPUs of the same model get their average. With no
  token able to measure them all within 60 s, the GPUs count as equal. The
  weights are kept for the session.
- A token that cannot mine gives its whole GPUs to the others too: with
  ELECTRIC waiting for its pot, all five GPUs mine PHOTON.
- Each GPU is held on its own (`SearchHold::set_gpu`, `Monitor::hold_gpu`),
  so a rig's GPUs mine different tokens at once.
- The screen's **Split** line says how the GPUs are shared now, such as
  "GPU 1, 2, 3, 4 PHOTON · GPU 5 ELECTRIC"; `--no-tui` prints it when it
  changes.

## Using it

In the setup, choose GPU mining, then **Hash split** at the token step. The
settings show one row per token: Left/Right moves its share by 5%. Raising a
share when the total is already 100% is refused with a note to lower another
token first. Under the last token: the total and the idle part.

With ELECTRIC above 0%, the setup makes its fee pot and recovery key as soon
as the payout is in and shows the pot's QR code, as for ELECTRIC alone.
#### PR #61: the pot's balance is read in the background as soon as the pot
shows, and every 30 s after, so funding it shows up on the page (for
ELECTRIC alone too). **Start reads the balance again**: a pot that can pay a
claim starts at once; an empty pot (or one that cannot be read) is reported,
and the next Start press starts anyway, with the other tokens mining
ELECTRIC's share until the pot is funded.

#### PR #61: the split and ELECTRIC save profiles like any other GPU
mining. The profiles file stays the one every build reads; which tokens
mine and their shares (`PHOTON=80` and so on, tokens at 0% left out) are
kept beside it in `profile-choices.json`, by profile name, and follow a
rename or delete. The profile list shows "Hash split PHOTON 80% · ELECTRIC
20%" or "ELECTRIC" for the token. A build without these choices opens such
a profile as plain PHOTON mining.

From the command line:

```text
pickaxe split --network mainnet --address <payout> --share photon=80 --share electric=20
```

Tokens not named get 0%. `--device`, `--backend`, `--source`, `--fulcrum`
and `--token-donation` apply as for `mine`; `--no-tui` prints each token's
rate, rewards and state every 30 seconds instead of the screen.

The screen lists each token's share, rate, rewards and state, the idle part,
the fee pot's balance (with its QR code while it cannot pay, `f` toggles it)
and recent events of every token. Up/Down picks a token and Left/Right (or
+/-) moves its share by 5% while mining. `q`, Esc or Ctrl+C stops every
token; pending claims are kept as they are without a split.

## Limits

- Local GPUs only: not for a rig of a coordinator, a farm's coordinator or a
  pool.
- Only tokens started with a share can be changed on the screen; a token at
  0% at the start does not run.
- ELECTRIC's replay weakness ([electric-token.md](electric-token.md)) is
  unchanged.

## Live check (2026-10-19)

Mainnet, RTX 5070 Ti Laptop GPU over CUDA, a release build of this branch,
`pickaxe split --share photon=80 --share electric=20 --no-tui` for about
three minutes, with no other miner running. Solo rates measured the same
day: PHOTON 1,688 MH/s (benchmark), ELECTRIC about 2,900 MH/s (dry run).

- ELECTRIC found a proof at 549.8 MH/s, 19% of its solo rate, and claimed it
  (`747be148…f824`), then the cheap claim after it at difficulty 3
  (`f0319b87…9464`); the network accepted both.
- ELECTRIC then waited for the next block, and PHOTON mined the whole round:
  1.64 to 1.67 GH/s, its solo rate.
- Stopped and restarted with the screen, the GPU stayed at 100%; no pending
  claim was left behind.

## Checks

- `hash_split::tests`: even shares and 5% steps that never pass 100%; the
  tokens available on each network and `--share` parsing; a round's plan
  (shares, a waiting token's time to the others, the idle part, nobody able
  to mine); the scheduler giving 80/20 within 5% over five rounds, PHOTON
  taking the round while ELECTRIC waits, and every hold released at stop;
  the screen with an unfunded and a funded pot.
- #### PR #61: `a_rig_gives_whole_gpus_first_and_turns_only_on_the_rest`
  (the table above and a 3:1 pair of GPUs) and
  `the_scheduler_measures_a_rig_then_gives_it_whole_gpus` (five GPUs of one
  model: every GPU takes turns while measuring, then GPUs 1 to 4 mine
  PHOTON and GPU 5 ELECTRIC with no turns, and every hold is released at
  stop).
- `search::tests::a_held_search_hashes_nothing_until_released_if_wgpu_present`
  (GPU): a held search does no work for 300 ms yet still wants its turn,
  resumes when released, and gives its turn away when paused. Passed on the
  RTX 5070 Ti Laptop GPU.
- `tui::tests::hash_split_setup_shares_the_gpus_and_checks_the_fee_pot_before_mining`:
  the token list entry, the share rows and their limit, the pot panel, Start
  without a payout, an empty pot warning once and starting on the next
  press, a funded pot starting at once, PHOTON alone with ELECTRIC at 0%,
  and no start with every share at 0%; #### PR #61: the profile's kept
  shares and their label, and a setup reopened with them.
- #### PR #61: `tui::tests::electric_setup_shows_the_fee_pot_to_fund_and_saves_its_profile`
  (the balance read while the pot shows) and
  `tui::tests::profile_choices_are_kept_renamed_and_dropped_with_their_profile`.
