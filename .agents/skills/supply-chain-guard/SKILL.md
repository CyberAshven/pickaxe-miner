---
name: supply-chain-guard
description: Use before cloning, checking out, building, linting, testing, installing or opening any repository, branch, PR head, package or download not written in this session, and before every commit and push. Scans git objects for injected loaders (PolinRider's whitespace-hidden config loader, Shai-Hulud install hooks, GlassWorm invisible Unicode, fake-font and editor-task launchers) without running anything, and stops the work when one is found.
---

# Supply-chain guard

On 2026-11-16 an obfuscated JavaScript loader was found in `eslint.config.mjs`
on CyberAshven/bchd (16 branches) and on the bitcoin-cash-explorer repos
(BitcoinCash1 `main`, CyberAshven `feat/p2p-light-client`). It sat at the end
of a line, after hundreds of spaces, so an editor showed a normal file. It
was delivered by rewriting existing commits (same message, author and author
date; new committer date in `+0000`) and force-pushing them under the
owner's account. Any `eslint` run would have executed it. It is the
PolinRider campaign, and the same families keep coming back. The rules below
keep such code from ever running here.

## What runs code without being asked

- **Tool config files.** eslint, babel, jest, postcss, tailwind, next, vite,
  webpack, commitlint and prettier load their config as code. `npm run
  lint/build/test`, `next build` or `jest` runs whatever the config holds.
- **Install hooks.** `npm install`, `npm ci`, `pnpm i`, `yarn` and `npx` run
  `preinstall`, `install`, `postinstall` and `prepare` from every package
  in the tree. Composer runs its `*-install-cmd` scripts.
- **Editors.** A `.vscode/tasks.json` task with `"runOn": "folderOpen"` runs
  when the folder is opened in VS Code or a fork of it. Extensions run code.
- **Rust builds.** `build.rs` and proc macros run on `cargo build`, `check`,
  `clippy` and `test`. A new crate, a new version or a git dependency is
  code that runs at build time.
- **Python and others.** `setup.py` runs on install; Docker `RUN` lines run
  on build; Makefiles and scripts run when called.
- **Downloads.** An installer, binary, archive script or `curl | sh` runs
  with the user's rights. A release tarball can differ from the tagged
  source, as with xz-utils in 2024.

## Rules

1. **Nothing fetched is run unread.** No `curl … | sh`, `bash <(curl …)`,
   `iwr … | iex`, `npx <package>` of an unchecked package, or `node`,
   `bun`, `deno` or `python` on a downloaded file. Binaries are verified
   against the checksums the project signs on its own release page (BCHN's
   `SHA256SUMS.asc`, for example) before they run.
2. **Scan before touching.** A repository, branch or PR head not written in
   this session is scanned with `scan.sh` (below) before it is checked out,
   installed, built, linted, tested or opened in an editor. The scan reads
   git objects only, so the scan itself runs nothing from the tree.
3. **JS tooling only on scanned trees, with install scripts off.**
   `npm ci --ignore-scripts` (`pnpm install --ignore-scripts`) against a
   committed lockfile. A new package is checked first:
   `npm view <pkg> time maintainers scripts`. A version published less than
   7 days ago is not adopted; hijacked releases are usually pulled within
   days. A changed maintainer or a new install script on an old package is
   a stop.
4. **Rust dependencies are reviewed.** `cargo deny check` passes, builds use
   `--locked`, and a new crate's `build.rs` and proc macros are read. No
   git dependencies on unreviewed branches.
5. **Editors open untrusted folders in Restricted Mode.** An agent never runs
   `code <dir>` on an unscanned tree. `task.allowAutomaticTasks` stays off.
6. **Every commit and push is scanned first.** `scan.sh . --staged` before
   committing, and `scan.sh . HEAD` before pushing. Malware on a machine can
   write into any working tree, an agent's included, so a loader in one's own
   diff is a stop, not a typo.
7. **Merges name the reviewed head.** `gh pr merge … --match-head-commit
   <sha>` with the SHA that was pushed or reviewed, never the branch name
   alone.
8. **A hit stops the work.**
   - Nothing on that tree is linted, built, run, installed, rebased,
     pushed or force-pushed.
   - Recorded: repository, ref, commit, file, line, committer, both dates,
     and the force-push entry if there is one.
   - Reported to the owner at once.
   - Restoring branches, rotating tokens, and auditing sessions, SSH keys,
     GitHub Apps and OAuth grants are the owner's work. If the code may
     already have run on this machine, that is said plainly: the machine
     then needs cleaning, not only the repository.
9. **Owner-side protections, to suggest, never to change.**
   - Branch protection that blocks force-pushes on `main`, `alpha`, `dev`
     and release branches.
   - Fine-grained tokens with an expiry.
   - 2FA with a security key.
   - A review of installed GitHub Apps and OAuth grants.
   - Windows Defender real-time protection on.

## Scan

`scan.sh` sits beside this file: `~/.claude/skills/supply-chain-guard/`
for the user-level copy, `.agents/skills/supply-chain-guard/` in a
repository. It needs git, and gh only for `--github`. It never checks out
the tree.

```sh
G=~/.claude/skills/supply-chain-guard/scan.sh   # or .agents/skills/supply-chain-guard/scan.sh

# A remote PR or branch, without a working tree:
git init --bare /tmp/scan.git
git -C /tmp/scan.git fetch https://github.com/OWNER/REPO.git \
    +refs/heads/main:refs/heads/main +refs/pull/15/head:refs/pr/15
bash $G /tmp/scan.git refs/pr/15 --range main..refs/pr/15 --github OWNER/REPO

bash $G . HEAD            # a local tree
bash $G . --staged        # the index, before a commit
bash $G . HEAD --since 30 # also list commits rewritten long after writing
```

Exit status: 2 for a HIT, 1 for REVIEW items only, 0 for clean.

| Check | Level | Catches |
|---|---|---|
| Loader markers (case-insensitive): `global\.o *= *['"]`, `global\[['"]!['"]\] *=`, `global\.i *= *['"]A8-`, `_\$_[0-9a-f]{4}`, PolinRider's `rmcej` seed, its Ethereum dead-drop address and its command paths under `/0x/` | HIT | PolinRider and our 2026-11-16 loader |
| Code after 150+ blanks on one line | HIT | Code hidden off the right edge of the editor |
| Run of 4+ variation selectors or private-use characters | HIT | GlassWorm's invisible payload |
| Run of 4+ Unicode tag characters | REVIEW | Text hidden for AI agents ("ASCII smuggling") |
| Text direction controls | REVIEW | Trojan Source |
| `"runOn": "folderOpen"` | HIT | Editor-task launchers (Contagious Interview, PolinRider) |
| A font or image file that git reads as text | HIT | Scripts disguised as `.woff2` fonts |
| `setup_bun.js`, `bun_environment.js`, a path named after Shai-Hulud | HIT | The Shai-Hulud worm's files |
| npm lifecycle scripts, composer install scripts | REVIEW | Install-time execution |
| `eval`, `Function(`, `createRequire`, `child_process`, `.unref()`, `atob`, `base64` in tool configs | REVIEW | Loaders that keep a valid-looking config |
| `.gitignore` lines hiding `.bat`, `.cmd`, `.ps1`, `.vbs`, `.exe` | REVIEW | Dropped payloads kept out of `git status` |
| `--range`: markers in every commit of the range | HIT | Code that was added and then hidden again |
| `--since`: committer date over a day after the author date | REVIEW | Rewritten history (rebases show up too) |
| `--github`: the repository's force-push log | REVIEW | Who rewrote which branch, and when |

REVIEW items are read before anything goes on. Rebases, cherry-picks,
upstream merges and Dependabot's force-pushes are normal. A force-push by
the owner's account that nobody in a session made is not.

## Known campaigns

| Campaign | How it got in | How it hides and runs | Goal |
|---|---|---|---|
| PolinRider (North Korea, 2026) | Stolen account access; rewritten commits force-pushed, sometimes with backdated dates; also through compromised npm, Packagist and Go packages | A loader appended after ~1,400–2,000 spaces to `eslint`, `tailwind`, `postcss`, `next`, `babel` or `commitlint` configs; fake `.woff2` fonts started by a `folderOpen` task; `.gitignore` hiding `config.bat`; command server looked up in an Ethereum transaction | Crypto wallets and credentials |
| Contagious Interview (North Korea) | Fake job offers with a coding test that needs `npm install` | BeaverTail, then the InvisibleFerret backdoor; fake fonts and editor tasks | Wallets, browser credentials |
| Shai-Hulud (npm worm, 2025–2026) | Trojanized releases of real packages, published with stolen npm tokens | `preinstall`/`postinstall` running `bundle.js`, `setup_bun.js` or `bun_environment.js`; TruffleHog to harvest tokens; republishes every package the token can reach | Tokens and keys; deletes home-folder files when it finds none |
| chalk/debug (September 2025) | A phished maintainer of chalk, debug and ~16 other packages | Browser code that swapped crypto addresses just before a transaction was signed | Payments sent to the attacker |
| GlassWorm (VS Code extensions, 2025) | Compromised extensions on the VS Code Marketplace and OpenVSX | Payload encoded in invisible Unicode; command server read from Solana transaction memos | Credentials, wallets, more extensions |
| Our incident (2026-11-16) | Rewritten commits force-pushed as CyberAshven | `global\.o='8-hhn11666';var _\$_6a5e=` after hundreds of spaces in `eslint.config.mjs` | Ran on any `eslint` call |

Bitcoin Cash projects are a target because these campaigns are after
wallets. A change that touches payment addresses, signing or wallet code
gets the same scrutiny as a config file.

## Sources

- Socket, *PolinRider spreads through compromised GitHub accounts and Packagist*: https://socket.dev/blog/polinrider-github-packagist
- OpenSourceMalware, *PolinRider npm case study*: https://opensourcemalware.com/blog/polinrider-npm-case-study-dprk-attack
- GitHub Community, *Targeted malware altering tailwind.config.js and .gitignore via force-push*: https://github.com/orgs/community/discussions/197873
- mnemonic, *Shai-Hulud 2.0 advisory*: https://www.mnemonic.io/resources/blog/advisory-shai-hulud-2.0-supply-chain-campaign-rapidly-spreading-through-npm-packages-and-github/
- StepSecurity, *chalk, debug and 18 more packages compromised*: https://www.stepsecurity.io/blog/20-popular-npm-packages-compromised-chalk-debug-strip-ansi-color-convert-wrap-ansi
- Endor Labs, *Invisible threats: GlassWorm*: https://www.endorlabs.com/reports/invisible-threats-glassworm-unicode-vscode
- Microsoft, *Contagious Interview malware delivered through fake developer job interviews*: https://microsoft.com/en-us/security/blog/2026/03/11/contagious-interview-malware-delivered-through-fake-developer-job-interviews
