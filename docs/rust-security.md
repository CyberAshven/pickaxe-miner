# Rust security checks

The required Linux CI job runs `bash tools/check-rust-deps.sh` on every PR. It
checks both the miner and standalone GPU crate with cargo-deny 0.20.2, fresh
RustSec advisories, locked dependencies and all miner features. Windows/Linux
formatting, Clippy, tests and Rust GPU compilation remain in the existing CI.

`deny.toml` rejects known vulnerabilities, soundness advisories, unmaintained
and yanked crates, unapproved licenses and unapproved dependency sources. Git
dependencies require a revision pin. There are no advisory suppressions.

Native dependencies are allowed and receive the same dependency checks. CI does
not select a mining backend or ban a library based on its implementation
language. Backend changes still require correctness and performance review.

## Staying current

Daily scans check both `master` and `dev`, including newly disclosed advisories
when no code has changed. Dependabot proposes weekly Cargo and GitHub Actions
updates into `dev`; reviews and CI are still required. Security-update PRs target
GitHub's default branch (`master`); bring merged fixes back into `dev` too.
No dependency updates are automatically merged.

GitHub activates schedules and reads `.github/dependabot.yml` from the default
branch. These automations become active after this configuration is reviewed,
merged into `dev`, and promoted to `master`. PR checks run before that. The
cargo-deny version is explicitly pinned in the check script and should be
updated through a reviewed PR. The GPU compiler remains pinned separately;
compiler changes still need GPU correctness and performance validation.

## Scope

These checks find known dependency problems and enforce dependency
policies; passing CI is not proof that software has no vulnerabilities. Native
GPU drivers, HIP kernels and platform TLS integration still exist. FFI safety,
mining correctness, hardware stability and sustained performance still require
review and end-to-end testing. This change does not replace the running miner
or publish a new release.

References: [cargo-deny checks](https://embarkstudios.github.io/cargo-deny/checks/index.html),
[RustSec](https://rustsec.org/), and
[Dependabot configuration](https://docs.github.com/en/code-security/concepts/supply-chain-security/about-the-dependabot-yml-file).
