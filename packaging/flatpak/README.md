# Flatpak packaging

`io.github.mmclinton.Spectral.yaml` builds the tree it sits in: a clean export of a commit when built with `cargo xtask package flatpak` (see [../README.md](../README.md)), or a working copy when run by hand.

## Build and install

Flatpak must be enabled on the host (`services.flatpak.enable = true` on NixOS), with the builder installed once:

```bash
flatpak remote-add --if-not-exists --user flathub https://dl.flathub.org/repo/flathub.flatpakrepo
flatpak install -y --user flathub org.flatpak.Builder
```

Then build, check and install a commit:

```bash
cargo xtask package flatpak
flatpak install --user target/packages/flatpak/pkg/io.github.mmclinton.Spectral.flatpak
flatpak run io.github.mmclinton.Spectral
```

Flatpak exports one command per application and names it after the app ID, so the CLI is on `$PATH` as `io.github.mmclinton.Spectral`, never as `spectral`. `flatpak build-finish` chooses which command runs (`--command=`) but cannot name the exported wrapper, and Flatpak requires exported files to carry the app ID anyway — a short name is a shell alias on the user's side.

```bash
io.github.mmclinton.Spectral status
```

To reach the daemon binary, which is not the exported command:

```bash
flatpak run --command=spectral-daemon io.github.mmclinton.Spectral
```

**Stop any host daemon first.** The sandbox gets a private `$XDG_RUNTIME_DIR`, so the Flatpak daemon and a natively installed one never see each other's socket — but they will both try to publish a virtual mic under the same node name, which makes any test result ambiguous.

## Regenerating cargo-sources.json

flatpak-builder builds with no network, so every crate is listed as an explicit source with a checksum. The script fetches everything it needs on demand and leaves nothing behind, so the dev shell carries no tooling for a target it never builds. Whenever `Cargo.lock` changes:

```bash
./packaging/flatpak/generate-sources.sh
```

Commit the result together with the `Cargo.lock` change that caused it.

## Permissions

The whole static permission set is four entries:

| Permission | Why |
| --- | --- |
| `--share=ipc` | MIT-SHM under X11 |
| `--socket=wayland` | the GUI |
| `--socket=fallback-x11` | the GUI, where Wayland is absent |
| `--filesystem=xdg-run/pipewire-0` | the only way to reach PipeWire; there is no `--socket=pipewire` |

Deliberately absent: `--device=dri` (`src/main.rs` forces Slint's software renderer, so there is no GPU path to grant), `--socket=pulseaudio` (Spectral speaks native PipeWire and never touches the compatibility socket), network access, home access, and any D-Bus name.

Note that `src/platform/linux/mod.rs::manager_client_properties` is what makes the sandboxed build work at all: WirePlumber grants an undeclared Flatpak client only `rx`, which cannot create the virtual mic or move the default input.

## Differences from the native builds

- **The CLI answers to the app ID**, `io.github.mmclinton.Spectral`, as described under [Build and install](#build-and-install).
- **Paths move under `~/.var/app/io.github.mmclinton.Spectral/`.** Settings and voice-test recordings live there rather than under `~/.config` and `~/.local/state`.
