# Development and releases

Feature branch -> reviewed PR into `dev` -> alpha testing -> reviewed PR into `master`.

Both long-lived branches require one independent approval, passing CI, resolved
review conversations and up-to-date branches. Code changes need fresh approval.
Force pushes and deletion are blocked. Use a normal merge commit for `dev` into
`master` promotions to preserve the shared history.

## Publish an alpha deliberately

Merging into `dev` does not publish a release. In a reviewed PR into `dev`, set
the root package version in `Cargo.toml` and `Cargo.lock` to an alpha version,
for example `0.0.2-alpha.1`. After that PR is merged and CI passes, tag the exact
reviewed commit (check its Cargo version first):

```bash
git fetch origin dev
git tag pickaxe-miner-v0.0.2-alpha.1 origin/dev
git push origin pickaxe-miner-v0.0.2-alpha.1
```

The existing Release workflow builds Windows/Linux archives, checksums and
provenance. Alpha releases are marked as prereleases and cannot replace the
latest stable release. Each alpha needs a new version/tag; do not move existing
tags. A tagged alpha must be part of `dev` history. Use the actual version chosen
for your release rather than copying the example number unchanged.

## Promote to stable

Complete end-to-end mining validation before promotion. Prepare a reviewed
version change on `dev` that removes the alpha suffix, for example `0.0.2`, then
open a PR from `dev` into `master`. Fresh approval and all required CI checks are
still required. After merging, the existing workflow publishes the new stable
version only if its tag does not already exist. Stable tags must belong to
`master` history. The release workflow rejects alpha versions on `master`.

Keep `dev` for subsequent work. Bring any master-only fixes back through a PR
into `dev`. Documentation-only changes do not start release publishing. Creating
this branch does not change the existing v0.0.1 release or publish an alpha.
