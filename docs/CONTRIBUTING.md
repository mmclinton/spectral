# Contributing

Spectral's public repository is https://github.com/mmclinton/spectral. Bug reports and patches are welcome as issues and pull requests there. The repository mirrors my own forge, so I merge accepted pull requests by hand, and they show as merged once the next sync reaches GitHub.

For a map of the codebase, read [ARCHITECTURE.md](ARCHITECTURE.md). What makes a bug report useful is listed under [Reporting a bug](TROUBLESHOOTING.md#reporting-a-bug).

## Building

Everything is pinned by the nix flake:

- `nix develop` — dev shell with the complete toolchain
- `nix build` — `result/bin/spectral` and `result/bin/spectral-daemon`
- `nix run` / `nix run .#daemon` — run the GUI or the daemon directly

A daemon outlives the client that started it, Ctrl-C on that client included, and a rebuild keeps the version, so no client replaces the running daemon ([IPC protocol](ARCHITECTURE.md#ipc-protocol)). Run `spectral shutdown` before trying a rebuilt daemon, or you are testing the old one.

## Before submitting

`nix flake check` must pass. It builds the package and gates formatting (`rustfmt.toml`), lints, and `cargo test`. The `[lints]` table in Cargo.toml denies every rustc and clippy warning, with the test carve-outs and the project's identifier vocabulary in `clippy.toml`; `cargo build` alone skips the clippy half.

If you touch Cargo.toml or Cargo.lock, also run `cargo deny check` from the dev shell (security advisories, license compliance, duplicate versions, registry sources — policy in `deny.toml`).

The MSRV is the `rust-version` field in Cargo.toml. It is bumped only when a change genuinely needs a newer compiler.

## Commits

Conventional Commits: `<type>(<scope>): <imperative description>`, kept small and independently reviewable. User-visible changes add a line under `[Unreleased]` in CHANGELOG.md.

## Licensing

GPL-3.0-only, inbound = outbound: by submitting a change you agree to license it under GPL-3.0-only. THIRD-PARTY.md accounts for the licenses of everything the binaries ship, the statically linked crates and the embedded font alike.

## Releases

Versioning, channels, and the release pipeline are documented in [RELEASING.md](RELEASING.md).
