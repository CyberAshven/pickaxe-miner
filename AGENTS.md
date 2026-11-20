# Agent instructions

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
