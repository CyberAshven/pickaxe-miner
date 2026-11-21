# Pickaxe updates

Pickaxe updates itself from its GitHub releases, as Gupaxx does in its
window and Claude Code does in the terminal: a quiet check, a one-line
notice, a download in the background, "restart to apply". P2Pool v2's own
updates are in the same place in the window.

## The trust rule

The GitHub account that publishes Pickaxe was used to push malicious code
in 2026, so a GitHub release on its own proves nothing: anyone holding the
account could publish one. A release is trusted only when its checksum file
(`SHA256SUMS.txt`) carries a [minisign](https://jedisct1.github.io/minisign/)
signature (`SHA256SUMS.txt.minisig`) from a public key compiled into
Pickaxe (`TRUSTED_KEYS` in `src/update/trust.rs`). The owner keeps the
secret key offline. It is never in the repository, on a build machine or in
CI.

| Release | What Pickaxe does |
|---|---|
| Signed with a compiled-in key, and the archive matches the signed checksum | Installs it by itself when Auto-update is on |
| No key compiled in, no signature, an unknown key, or a signature that does not verify | Shows it, never installs it by itself. U, the Update button or `pickaxe update` say **Unsigned release** and why, then ask before installing |

In every case:

- The archive's SHA-256 must match the line for it in `SHA256SUMS.txt`
  and GitHub's own record of the asset (its `digest`), when GitHub gives one.
  The archive name carries the version, so a signed checksum file from an
  older release cannot be passed off as a newer one.
- Downloads use https only, from `api.github.com` and `github.com`, and
  follow redirects only to GitHub's release file hosts
  (`release-assets.githubusercontent.com`, `objects.githubusercontent.com`).
- The archive may be no larger than the size GitHub lists for it, and never
  more than 512 MiB. The release description may be at most 1 MiB, the
  checksum file 64 KiB and the signature 4 KiB.
- Nothing is unpacked before the SHA-256 matches. Nothing from the archive
  runs before the next start, and only after it is in place.
- Only minisign's default signatures (pre-hashed, `ED`) are accepted. Legacy
  signatures (`minisign -l`) are refused.

This build ships with `TRUSTED_KEYS` empty, so no release installs by
itself until the owner adds the key.

## What you see

**Terminal.** One line on the right of the top border of the setup and the
GPU dashboards, only when there is news:

- `Pickaxe v0.0.5 available · U to update`
- `Pickaxe v0.0.5 available · Unsigned release: update manually (U)`. U
  says why it is unsigned and asks. U again installs it, X keeps the version
  you have.
- `Updating… 42%`
- `Updated to v0.0.5 · restart to apply`

X hides the notice until there is other news. While mining, U only
downloads: Pickaxe never restarts a session by itself. On the profiles
screen, where no session runs, U restarts into the installed version. The
**Pickaxe updates** row at the end of every profile's Advanced section
switches between on, notify and off. It is saved for the whole app, not for
the profile.

**Window.** Every top bar has an **Updates** button, with a dot while there
is news. Its panel shows this version, the newest one with its release
notes, whether it is signed, **Update** (an unsigned release asks first and
says why), progress, **Restart Pickaxe** (only between sessions) and the
Auto-update and checks boxes. Below that is **P2Pool v2**: the installed and
newest versions, whether the release is signed, **Update P2Pool**,
**Roll back** and P2Pool's own auto-update. This section uses P2Pool's
existing updater ([p2pool.md](p2pool.md)).

## When it checks

Pickaxe checks once at start and then every 6 hours, with one request to
`releases/latest` that carries the last answer's ETag. An unchanged answer
is a 304 and costs nothing against GitHub's rate limit. A start within 15
minutes of the last check reuses that check. A newer release also needs its
checksum file and signature, which are downloaded once and kept. Nothing
else polls.

`--no-update-check` turns checks, installs and the swap off for one run.
`pickaxe update set --check off` turns checks off for good.

## Installing and applying

An update is downloaded and checked, then unpacked into
`updates/versions/<tag>/` beside the configuration (on Windows
`%APPDATA%\Pickaxe\updates\`) and recorded in `updates/installed.json` as
pending. At the next start of `pickaxe mine` or `pickaxe gui`, or when you
press restart between sessions, the pending version takes the program's
place:

1. What runs now is kept in `versions/` to roll back to. For the first
   update from a version you installed by hand, its files are copied there
   first.
2. Each file and folder of the new package (`pickaxe.exe`, `cuda/`,
   `hip/`, `README.md`, the licenses) in the program's folder is renamed
   aside into `.pickaxe-old-<time>/`. Windows lets a running program be
   renamed, but not overwritten. Then the new one is copied in. If any step
   fails, the old files are put back.
3. The new program starts with the same command line. On Windows, the old
   process waits for it in the terminal. The files set aside are removed at
   a later start, once nothing runs from them.

The swap waits, and says why, when another Pickaxe is running or a claim is
pending: an ELECTRIC pot journal (`*.pending.json`) holding a claim, or a
coordinator's `*.rig-winners.json` holding unclaimed winners. It also waits
when the program is not a release package's `pickaxe(.exe)`, such as a
development build.

## Commands

```
pickaxe update                      check now and install a newer release
pickaxe update --check              only say whether one is out
pickaxe update --yes                install an unsigned release without asking
pickaxe update status               running, installed, previous, last check, settings
pickaxe update rollback             the previous version starts next time
pickaxe update set --auto-update on|off
pickaxe update set --check on|off
```

Auto-update is on by default when the build has a signing key. Otherwise it
is notify: Pickaxe tells you, and you install.

## For the owner: signing releases

All of this happens on an offline computer that holds the secret key. The
secret key never goes on this repository, a build machine or CI.

**Once: make the key.**

```
minisign -G -p pickaxe-release.pub -s pickaxe-release.key
```

Give it a strong password and keep a backup of `pickaxe-release.key` that is
also offline. The second line of `pickaxe-release.pub` (it starts with
`RW`) is the public key. Put it in `TRUSTED_KEYS` in `src/update/trust.rs`
in a normal reviewed commit. Releases from that version on can update by
themselves. That first version itself is installed by hand, because older
versions have no key.

**Each release.** After the release workflow has published the release:

1. Download `SHA256SUMS.txt` and the archives. Check them:
   `sha256sum -c SHA256SUMS.txt`, and the build provenance with
   `gh attestation verify <archive> --repo CyberAshven/pickaxe-miner`.
   Sign only a release you checked.
2. On the offline computer, sign the checksum file:

   ```
   minisign -Sm SHA256SUMS.txt -s pickaxe-release.key -t "pickaxe-miner-v0.0.5"
   ```

   This writes `SHA256SUMS.txt.minisig`. The trusted comment (`-t`) is
   signed too, so name the release in it.
3. Check the signature: `minisign -Vm SHA256SUMS.txt -p pickaxe-release.pub`.
4. Upload it: `gh release upload pickaxe-miner-v0.0.5 SHA256SUMS.txt.minisig`.

A release whose download set is rebuilt later gets a new `SHA256SUMS.txt`,
which needs a new signature.

**Changing the key.** Add the new public key next to the old one in
`TRUSTED_KEYS` and release that version signed with the old key. Sign later
releases with the new key, then remove the old key in a later version. If a
secret key is lost or exposed, remove its public key in a release that you
sign with the remaining key. Without one, users install that release by
hand.

**P2Pool v2.** `P2POOL_TRUSTED_KEYS` in the same file does the same for
P2Pool's releases, if they publish `SHA256SUMS` (or `SHA256SUMS.txt`) and its
`.minisig`. When a key is set there, such a release installs only when its
signature verifies and its archive matches. Until then, P2Pool installs
against its published `.sha256` alone and shows as unsigned.
