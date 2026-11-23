# Packaging

Every package built from this repository stages its files through one list. The recipes live here, one folder per format, and none of them names a source URL or a hash: whoever builds a recipe exports the wanted commit first, so a recipe inside a release tag builds that tag.

| Format | Recipe | Build and check |
| --- | --- | --- |
| Nix | [`nix/package.nix`](../nix/package.nix) | `nix build` |
| Flatpak | [`flatpak/io.github.mmclinton.Spectral.yaml`](flatpak/io.github.mmclinton.Spectral.yaml) | `cargo xtask package flatpak` |
| Arch | [`arch/PKGBUILD`](arch/PKGBUILD) | `cargo xtask package arch` |
| Debian, Ubuntu | [`deb/debian/`](deb/debian) | `cargo xtask package deb` |

## The files

`cargo xtask install` is the list (`xtask/src/install.rs`). It installs and never builds:

```bash
cargo build --release --locked
cargo xtask install --destdir "$pkgdir" --prefix /usr
```

`--licensedir` moves the license files (default `<prefix>/share/licenses/spectral`), and `--app-only` leaves out the man page and the shell completions. The binaries are looked for in the `release/` folder of the target directory xtask itself was built into; a build given `--target <triple>` puts them in `target/<triple>/release` instead, which `--binaries <dir>` names. A new file in `assets/` fails the tests until the list installs it or names it as deliberately left out.

Dependencies are declared per format, because package names differ between distributions. The libraries winit loads at run time (`dlopenLibs` in `nix/package.nix`) are invisible to the tools that work out dependencies from the binary, so the Nix, Arch and deb recipes name them by hand; the Flatpak gets them from its runtime.

## Building and checking a package

```bash
cargo xtask package <flatpak|arch|deb> [--ref <git-ref>]
```

It exports the ref (default `HEAD`, so commit first) with `git archive`, builds the package from that export, and runs the format's linter (`flatpak-builder-lint`, `namcap`, `lintian`), whose findings are for a person to triage. It then installs the package into a fresh container: through the package manager for Arch and deb, so only the declared dependencies arrive, and with `flatpak install` for the Flatpak bundle, whose runtime comes from Flathub. Each later step starts a fresh container from the installed system: it checks `spectral --version`, starts PipeWire and WirePlumber with a test tone as the only input device and checks that `spectral enable` brings up a virtual mic that is running, linked at both ends and the default source, and launches the GUI headless against X11 and against Wayland: its window must appear and stay up. The unsigned package is left in `target/packages/<format>/pkg/`, and a pass or fail line per step closes the run.

It needs rootless podman, and for the Flatpak also Flatpak with `org.flatpak.Builder`. An interrupted run can leave its containers running; the next run of the same format removes them. To try the Flatpak build: `flatpak install --user target/packages/flatpak/pkg/io.github.mmclinton.Spectral.flatpak`.

## Versions

The version spots, and why the deb and the Flatpak have none, are in [docs/RELEASING.md](../docs/RELEASING.md).
