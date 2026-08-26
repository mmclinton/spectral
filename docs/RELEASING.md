# Releasing

How Spectral versions are decided, cut, and delivered. This document is
the single authority on the release process: where any other document,
script comment, or habit disagrees with it, this document wins, and the
fix is a patch to this file.

## Channels

A release reaches users through exactly two channels:

| Channel | What a release means there |
| --- | --- |
| nixpkgs (`spectral-mic`) | an update PR pinning the new tag and hashes |
| Flathub | a manifest bump in the Flathub app repository (once the initial submission lands) |

**Deliberately out of scope:** the AUR, deb, rpm, PPA/COPR
repositories, AppImage, raw release tarballs, and prebuilt binaries.
Flatpak covers every desktop with a pinned runtime and an update
cadence this project controls; distro archives would freeze a
fast-moving pre-1.0 app into stale packages; and a binary built on one
machine inherits its loader and library paths and fails on everyone
else's. The list is a decision, not an oversight — concrete demand can
reopen it, and anyone is welcome to package the release tags
independently (on the AUR, note that the name `spectral` is taken by
an unrelated tool; this project used `spectral-mic` there).

## When to release

Releases are milestone-driven: a release is cut when a coherent,
user-visible arc has landed — a completed feature, a batch of fixes
worth users' download — not on a calendar and not per commit.

Pushing `main` releases nothing. Every channel pins an annotated tag
plus a content hash, so `main` is free: commit, push, iterate daily
with no downstream consequence. The corollary with teeth: **a published
tag is immutable.** Deleting or re-pointing one breaks the pinned
hashes in every downstream rebuild. A bad release is fixed by a new
patch release, never by retagging.

The per-change test:

> Does someone who installed the package benefit from receiving this?

- **No** — comments, refactors, CI, docs, packaging expressions: land
  it on `main` and stop. It ships silently inside the next release.
- **Yes** — it gets a line under `[Unreleased]` in CHANGELOG.md when it
  lands, and urgency decides the timing. The default is to batch;
  only breakage and security justify a same-day release. UI wording is
  user-visible (it is compiled into the binary); a code comment is not.
- **Packaging-only error** (a package expression or manifest wrong,
  source fine) — no new tag. nixpkgs: a package-only PR. Flathub: a
  manifest-only PR in the Flathub repository.

`[Unreleased]` is both the accumulator and the gauge: when it reads
like something a user would want, cut the release.

## Versioning

Pre-1.0 SemVer: any feature in the batch makes a minor (`0.X.0`);
fixes only make a patch. Tags are annotated — `git tag -a vX.Y.Z` with
the title `Spectral X.Y.Z` and `-` bullets summarizing the release.

## The version-bump surface

Four files carry the version and must move together:

| File | Field | Moved by |
| --- | --- | --- |
| `Cargo.toml` | `[package] version` | `packaging/bump-version.sh` |
| `Cargo.lock` | the `spectral` stanza | the script, via cargo |
| `nix/package.nix` | `version` | the script |
| `assets/page.codeberg.mmc.Spectral.metainfo.xml` | `<release>` entry | **authored by hand first** — the script only verifies it exists |

The in-repo Flatpak manifest builds `main` and carries no version; the
versioned manifest lives in the Flathub repository. `nix/package.nix`
has a `versionCheckHook`, so `nix flake check` fails a cut whose binary
and expression disagree.

## Cutting a release

1. Author the metainfo `<release version="X.Y.Z" date="YYYY-MM-DD">`
   entry — before anything else; it ships inside the tarball. Validate:
   `appstreamcli validate assets/*.metainfo.xml`.
2. `./packaging/bump-version.sh X.Y.Z`
3. Roll CHANGELOG.md: `[Unreleased]` → `[X.Y.Z] - date`, keep an empty
   `[Unreleased]` above, fix the compare links at the bottom
   (`/compare/vA...vB`). If `Cargo.lock` moved since the last release,
   check THIRD-PARTY.md against it (the recipe is at that file's foot)
   and regenerate `packaging/flatpak/cargo-sources.json`
   (`./packaging/flatpak/generate-sources.sh`).
4. Gates: `nix flake check` and, from the dev shell, `cargo deny check`
   (needs network for the advisory DB). Any advisory hit is triaged
   before the release commit, never after.
5. Build the release candidates both ways — `nix build` and the
   Flatpak dev manifest — and **validate them functionally**: the
   virtual mic audibly denoises on a real device. No automated check
   substitutes for this.
6. Commit `chore(release): vX.Y.Z`, tag `git tag -a vX.Y.Z`, verify
   `git describe --tags` prints exactly `vX.Y.Z` on a clean tree, and
   push with `git push --follow-tags`.

## Fan-out

Order: nixpkgs first (it merges on reviewer time), then Flathub.
Skipping versions downstream is legal — each channel bumps from
whatever it has to the newest tag.

### nixpkgs

Preflight: one open PR per package, never two.

- An earlier bump PR is still open — update it in place (rebase, redo
  the hashes, amend, retitle `spectral-mic: OLD -> X.Y.Z` where OLD is
  what nixpkgs master holds) rather than opening a second.
- The r-ryantm bot may open the bump first — review and approve that
  PR instead of competing with it.

Fresh update: branch from `upstream/master`, set `version`, set both
`src.hash` and `cargoHash` to `lib.fakeHash` (`cargoHash` moves even
when `Cargo.lock` looks unchanged — its own version line is enough),
and let `nix-build -A spectral-mic` report the real hashes one mismatch
at a time. A 404 is not a mismatch — it means the tag is missing.
Smoke-test `./result/bin/`, run the tree's formatter and
`./ci/nixpkgs-vet.sh master`, commit as `spectral-mic: OLD -> NEW` with
the changelog URL
(`https://codeberg.org/mmc/spectral/src/tag/vX.Y.Z/CHANGELOG.md`) in
the body, and open the PR with only truthful checkboxes. A later
force-push or body edit notifies nobody — only a comment does.

**Parity:** general improvements to either package expression are
mirrored to the other in the same session. Must stay in sync: runtime
and build inputs, `postInstall`, `__structuredAttrs` + `env.` vars,
renamed nixpkgs attrs, the `versionCheckHook` block, and `meta`.
Intentionally different (do not sync): the builder
(`rustPlatform.buildRustPackage` vs crane), the toolchain, `src`
(`fetchFromGitea` vs a fileset), `cargoHash` (crane vendors from the
lockfile), where tests run, `pname` (`spectral-mic` vs `spectral`),
and the `passthru`/maintainers/fixpoint plumbing.

### Flathub

Bump the manifest's source tag in the Flathub app repository and
refresh its cargo sources if the lockfile moved. Not applicable until
the initial submission lands.

## Propagation

What users see, when, after a release:

1. **nixpkgs `master`** — when the bump PR merges; days to weeks of
   review latency is normal, and after about a week a polite post in
   the Discourse "PRs ready for review" thread is the escalation path.
2. **`nixos-unstable`** — a few days after merge, once Hydra builds it.
3. **Flathub** — hours after the Flathub PR merges.
4. **NixOS stable** — only at the next release branch-off; backports
   are for critical and security fixes only. Stable users being one
   release behind is the channel's contract, not a problem to solve.
