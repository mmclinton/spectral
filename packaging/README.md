# Packaging

Every package of Spectral installs the same files, from one list. The recipes live here, one folder per format, and none of them names a source URL or a hash: whoever builds a recipe exports the wanted commit first, so a recipe inside a release tag builds that tag.

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

`--licensedir` moves the license files (default `<prefix>/share/licenses/spectral`), and `--app-only` leaves out the man page and the shell completions. A new file in `assets/` fails the tests until the list installs it or names it as deliberately left out.

Dependencies are declared per format, because package names differ between distributions. The libraries winit loads at run time (`dlopenLibs` in `nix/package.nix`) are invisible to the tools that work out dependencies from the binary, so every recipe names them by hand.

## Building and checking a package

```bash
cargo xtask package <flatpak|arch|deb> [--ref <git-ref>]
```

It exports the ref (default `HEAD`, so commit first) with `git archive`, builds the package from that export, and runs the format's linter (`flatpak-builder-lint`, `namcap`, `lintian`), whose findings are for a person to triage. For Arch and deb it then installs the package into a fresh container through the package manager, so only the declared dependencies arrive, checks `spectral --version`, and launches the GUI headless against X11 and against Wayland: the GUI must still be running after eight seconds. The unsigned package is left in `target/packages/<format>/pkg/`, and a pass or fail line per step closes the run.

It needs rootless podman for Arch and deb, and Flatpak with `org.flatpak.Builder` for the Flatpak. To try the Flatpak build: `flatpak install --user target/packages/flatpak/pkg/io.github.mmclinton.Spectral.flatpak`.

## Versions

`bump-version.sh` moves the PKGBUILD's `pkgver` with every other version spot. The deb's changelog is generated from the crate version at build time, and the Flatpak manifest carries no version.
