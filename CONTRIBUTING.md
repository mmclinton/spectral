# Contributing

Spectral's public repository is https://github.com/mmclinton/spectral. Bug reports and patches are welcome as issues and pull requests there. The repository mirrors my own forge, so I merge accepted pull requests by hand, and they show as merged once the next sync reaches GitHub.

For a map of the codebase, read [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md). What makes a bug report useful is listed under [Reporting a bug](docs/TROUBLESHOOTING.md#reporting-a-bug).

## Building

Everything is pinned by the nix flake:

- `nix develop` — dev shell with the complete toolchain
- `nix build` — `result/bin/spectral` and `result/bin/spectral-daemon`
- `nix run` / `nix run .#daemon` — run the GUI or the daemon directly

A daemon outlives the client that started it, Ctrl-C on that client included, and a new client reuses whichever daemon is already running unless its protocol version differs. Run `spectral shutdown` before trying a rebuilt daemon, or you are testing the old one.

## Before submitting

`nix flake check` must pass. It builds the package and gates formatting (`rustfmt.toml`), lints, and `cargo test`. The `[lints]` table in Cargo.toml denies every rustc and clippy warning, with the test carve-outs and the project's identifier vocabulary in `clippy.toml`. A bare `cargo build` enforces the rustc half only, because rustc ignores `clippy::` lints; the clippy half needs `cargo clippy`.

If you touch Cargo.toml or Cargo.lock, also run `cargo deny check` from the dev shell (security advisories, license compliance, duplicate versions, registry sources — policy in `deny.toml`).

The MSRV is the `rust-version` field in Cargo.toml. It is bumped only when a change genuinely needs a newer compiler.

## Commits

Conventional Commits: `<type>(<scope>): <imperative description>`, kept small and independently reviewable. User-visible changes add a line under `[Unreleased]` in CHANGELOG.md.

## Licensing

GPL-3.0-only, inbound = outbound: by submitting a change you agree to license it under GPL-3.0-only. Third-party notices for everything shipped in the binaries — the statically linked crates and the embedded font alike — live in THIRD-PARTY.md.

## Releases

Versioning, channels, and the release pipeline are documented in [docs/RELEASING.md](docs/RELEASING.md). I cut a release when a coherent user-visible arc has landed; landing on `main` alone ships nothing.
