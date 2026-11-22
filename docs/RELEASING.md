# Releasing

How Spectral versions are decided, cut, and delivered. This document is the single authority on the release process: where any other document, script comment, or habit disagrees with it, this document wins, and the fix is a patch to this file.

## Channels

A release reaches users through:

| Channel | What a release means there |
| --- | --- |
| nixpkgs (`spectral-mic`) | an update PR pinning the new tag and hashes |

**Deliberately out of scope:** rpm, PPA and COPR repositories, the AUR, AppImage, raw release tarballs, and prebuilt binaries. A binary built on one machine inherits its loader and library paths and fails on everyone else's. The list is a decision, not an oversight: concrete demand can reopen it, and anyone is welcome to package the release tags independently.

## When to release

Releases are milestone-driven: a release is cut when a coherent, user-visible arc has landed — a completed feature, a batch of fixes worth users' download — not on a calendar and not per commit.

Pushing `main` cuts no release: every channel pins an annotated tag plus a content hash. The flake is the exception, since `github:mmclinton/spectral` follows `main`: a push reaches its users' next build, though a daemon they already have running keeps the old one until it restarts ([INSTALLATION.md](INSTALLATION.md#nix)). The corollary of pinning, with teeth: **a published tag is immutable.** Deleting or re-pointing one breaks the pinned hashes in every downstream rebuild. A bad release is fixed by a new patch release, never by retagging.

The per-change test:

> Does someone who installed the package benefit from receiving this?

- **No** — comments, refactors, CI, docs, packaging expressions: land it on `main` and stop. It ships silently inside the next release.
- **Yes** — it gets a line under `[Unreleased]` in CHANGELOG.md when it lands, and urgency decides the timing. The default is to batch; only breakage and security justify a same-day release. UI wording is user-visible (it is compiled into the binary); a code comment is not.
- **Packaging-only error** (a package expression or manifest wrong, source fine) — no new tag. nixpkgs: a package-only PR. The recipes in `packaging/` build the tag they sit in, so a fix to one of them ships in a new patch release.

`[Unreleased]` is both the accumulator and the gauge: when it reads like something a user would want, cut the release.

## Versioning

Pre-1.0 SemVer: any feature in the batch makes a minor (`0.X.0`); fixes only make a patch. Tags are annotated — `git tag -a vX.Y.Z` with the title `Spectral X.Y.Z` and `-` bullets summarizing the release.

## The version-bump surface

These files carry the version and must move together:

| File | Field | Moved by |
| --- | --- | --- |
| `Cargo.toml` | `[package] version` | `packaging/bump-version.sh` |
| `Cargo.lock` | the `spectral` stanza | the script, via cargo |
| `nix/package.nix` | `version` | the script |
| `packaging/arch/PKGBUILD` | `pkgver` (and `pkgrel` back to 1) | the script |
| `assets/spectral.1` | the `.TH` line's `"spectral X.Y.Z"` field | the script, preserving the page's own revision date |
| `assets/io.github.mmclinton.Spectral.metainfo.xml` | `<release>` entry | **authored by hand first** — the script only verifies it exists |

The Flatpak manifest builds the tree it sits in and carries no version, and the deb's changelog is generated from the crate version, so neither is a spot. `nix/package.nix` has a `versionCheckHook`, so `nix flake check` fails a cut whose binary and expression disagree, and unit tests pin the man page's `.TH` version and the PKGBUILD's `pkgver` to the crate's, so the same gate catches those spots.

## Cutting a release

1. Author the metainfo `<release version="X.Y.Z" date="YYYY-MM-DD">` entry — before anything else; it ships inside the tarball. Validate: `appstreamcli validate assets/*.metainfo.xml`.
2. `./packaging/bump-version.sh X.Y.Z`
3. Roll CHANGELOG.md: `[Unreleased]` → `[X.Y.Z] - date`, keep an empty `[Unreleased]` above, fix the compare links at the bottom (`/compare/vA...vB`). If `Cargo.lock` moved since the last release, check THIRD-PARTY.md against it (the recipe is at that file's foot) and regenerate `packaging/flatpak/cargo-sources.json` (`./packaging/flatpak/generate-sources.sh`).
4. Gates: `nix flake check` and, from the dev shell, `cargo deny check` (needs network for the advisory DB). Any advisory hit is triaged before the release commit, never after.
5. Commit `chore(release): vX.Y.Z`. `cargo xtask package` builds a commit, never the working tree, so the candidates have to come from this one.
6. Build the release candidates both ways — `nix build` and `cargo xtask package flatpak` (install the bundle it leaves in `target/packages/flatpak/pkg/`) — and **validate them functionally**: the virtual mic audibly denoises on a real device. No automated check substitutes for this. A fix found here is a further commit, and the candidates are built again from it.
7. Tag `git tag -a vX.Y.Z`, verify `git describe --tags` prints exactly `vX.Y.Z` on a clean tree, and push with `git push --follow-tags`.

## Fan-out

Skipping versions downstream is legal: a channel bumps from whatever it has to the newest tag.

### nixpkgs

Preflight: one open PR per package, never two.

- An earlier bump PR is still open — update it in place (rebase, redo the hashes, amend, retitle `spectral-mic: OLD -> X.Y.Z` where OLD is what nixpkgs master holds) rather than opening a second.
- The r-ryantm bot may open the bump first — review and approve that PR instead of competing with it.

A fresh update:

1. Branch from `upstream/master` and set `version`.
2. Set both `src.hash` and `cargoHash` to `lib.fakeHash`, and let `nix-build -A spectral-mic` report the real hashes one mismatch at a time. `cargoHash` moves even when `Cargo.lock` looks unchanged, since its own version line is enough. A 404 is not a mismatch: it means the tag is missing.
3. Smoke-test `./result/bin/`, then run the tree's formatter and `./ci/nixpkgs-vet.sh master`.
4. Commit as `spectral-mic: OLD -> NEW`, with the changelog URL (`https://github.com/mmclinton/spectral/blob/vX.Y.Z/CHANGELOG.md`) in the body.
5. Open the PR with only truthful checkboxes. A later force-push or body edit notifies nobody; only a comment does.

**Parity:** an improvement to either package expression, this repository's or nixpkgs', is mirrored to the other right away.

- Keep in sync: runtime and build inputs, the installed files and the `wrapProgram` call, `__structuredAttrs` and `env.` vars, renamed nixpkgs attributes, the `versionCheckHook` block, and `meta`.
- Leave different: the builder (`rustPlatform.buildRustPackage` and its `cargoHash`, against crane vendoring from the lockfile), how the files are installed (`install` lines in nixpkgs' `postInstall`, against `cargo xtask install`), the toolchain, `src` (`fetchFromGitHub`, against a fileset), where tests run, `pname` (`spectral-mic`, against `spectral`), and the `passthru`, maintainer and fixpoint plumbing.

## Propagation

What users see, when, after a release:

1. **nixpkgs `master`** — when the bump PR merges; days to weeks of review latency is normal, and after about a week a polite post in the Discourse "PRs ready for review" thread is the escalation path.
2. **`nixos-unstable`** — a few days after merge, once Hydra builds it.
3. **NixOS stable** — only at the next release branch-off; backports are for critical and security fixes only. Stable users being one release behind is the channel's contract, not a problem to solve.
