# Contributing

Spectral is developed on Codeberg: https://codeberg.org/mmc/spectral.
Bug reports and patches are welcome via issues and pull requests there.

## Building

Everything is pinned by the nix flake:

- `nix develop` — dev shell with the complete toolchain
- `nix build` — `result/bin/spectral` and `result/bin/spectral-daemon`
- `nix run` / `nix run .#daemon` — run the GUI or the daemon directly

## Before submitting

`nix flake check` must pass. It builds the package and gates formatting
(`rustfmt.toml`), lints (all rustc and clippy warnings are denied — the
`[lints]` table in Cargo.toml means a bare `cargo build` enforces the
same), and `cargo test`.

If you touch Cargo.toml or Cargo.lock, also run `cargo deny check` from
the dev shell (security advisories, license compliance, duplicate
versions, registry sources — policy in `deny.toml`).

The MSRV is the `rust-version` field in Cargo.toml. It is bumped only
when a change genuinely needs a newer compiler.

## Commits

Conventional Commits: `<type>(<scope>): <imperative description>`, kept
small and independently reviewable. User-visible changes add a line
under `[Unreleased]` in CHANGELOG.md.

## Licensing

GPL-3.0-only, inbound = outbound: by submitting a change you agree to
license it under GPL-3.0-only. Third-party notices for statically
linked components live in THIRD-PARTY.md.
