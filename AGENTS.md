# Agent instructions

## What Pickaxe is

A Bitcoin Cash mining hub, like Gupaxx for Monero: one program with a desktop
window (`pickaxe-gui`, egui) and a terminal (`pickaxe`, ratatui). They share
one setup and one set of sessions. There is also a browser miner, packages for
mining OSes, and a Docker image ([README](README.md), [guide](docs/guide.md)).

- **GPU tokens** (CashTokens mined on GPUs): PHOTON is a token with direct
  rewards, ELECTRIC is a token that uses a fee pot, and the hash split mines
  two tokens at once.
- **ASICs**: Pickaxe is a **Stratum V1/V2 server and proxy that ASICs connect
  to**, never "an ASIC miner". The ASICs mine BCH, and ASIC tokens such as SAFA
  through merge mining.
- **Hosting**: a P2Pool v2 node, and GPU farms (a coordinator with rigs).
- **Nodes**: BCHN or Knuth over JSON-RPC, with Fulcrum as the fallback
  ([node requirements](docs/node-requirements.md)).
- **Names in the setup**: Solo, Public Solo Endpoint, Non-custodial Pool,
  P2Pool v2, Join a pool. A pool is people sharing rewards.

## How work is done here

- **Rust first.** New code is Rust. Add no JavaScript or npm dependencies,
  and shrink the existing web code where Rust can take its place. Use the
  SRI crates for Stratum V2 rather than writing protocols again.
- **Proper, not shortcuts.** Don't add a weaker mode for compatibility: no
  unencrypted SV2 links (Noise only), no unsigned or unverified downloads, no
  skipped checks. If the proper path waits on someone else (for example a
  node release), say so and wait.
- **One code path.** Mainnet and Chipnet run the same code with a network
  switch. The window and the terminal drive the same setup and sessions
  (`tui::SetupFlow`, `tui::SessionView`), and new features go in both.
- **Security findings.** Fix CodeQL alerts the way their own Recommendation
  says. If a protocol fixes the flagged value, explain that to the owner;
  never hide it from the scanner. Never dismiss alerts or bypass review
  rules.
- **Third-party code.** Study other miners and libraries, but write our own
  code. Don't run their binaries. P2Pool v2 is a separate upstream that
  Pickaxe installs and runs; don't change it from here.
- **Dependencies**: move to the newest releases (`--locked` builds, the
  `deny.toml` policy).
- **Comments** on anything non-obvious use the project's block: `What:`,
  `Why:`, `Look here if:`.
- **Before every push**, all must pass:
  - `cargo fmt --check`;
  - `cargo clippy --locked --all-targets -- -D warnings`, both with
    `--all-features` and with the default features;
  - `cargo check --locked --all-targets --no-default-features --features tail-grind`
    (the legacy build CI also runs);
  - the tests of the code you touched;
  - the supply-chain scan below.
- **Commits and PRs** describe the change and how it was checked. No
  attribution lines, no AI or tool mentions, no personal names. Docs change
  with the behaviour they describe.
- **Merging and releases are the owner's.** PRs need the owner's review.
  Promotions to `master` and version changes happen only when the owner asks.

## Skills

Skills live in `.agents/skills/<name>/`, each linked from
`.claude/skills/<name>` with a relative link
(`../../.agents/skills/<name>`).

- `supply-chain-guard` -- scan any branch, PR head, package or download
  not written in the session, and every commit and push, for injected
  loaders before anything runs; a hit stops the work

Read the matching skill before working in its area.

## Supply chain

Pickaxe handles wallets, payout addresses and signing, which is what the
2026 loader campaigns are after. In short (the skill has the details):

- `bash .agents/skills/supply-chain-guard/scan.sh . --staged` before every
  commit, and `scan.sh . HEAD` before every push. Exit status 2 is a hit:
  stop and report it.
- A branch or PR head written elsewhere is scanned before it is checked
  out, built, tested or opened in an editor.
- JS installs run as `npm ci --ignore-scripts` against the committed
  lockfile. Rust builds use `--locked`; a new crate's `build.rs` and proc
  macros are read first, and `tools/check-rust-deps.sh` passes.
- Nothing downloaded is run unread; binaries are checked against signed
  checksums first.
- The `Supply-chain guard` CI job scans every pull request with the base
  branch's scanner. A red result there is a hit, not a flake.
