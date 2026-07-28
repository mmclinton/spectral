# Flatpak packaging

`page.codeberg.mmc.Spectral.yaml` is the **development** manifest: it builds
the checked-out repository so the Flatpak can be tested before anything is
published. The Flathub submission uses a near-identical manifest that differs
only in its source entry — see [For Flathub](#for-flathub) below.

## Prerequisites

Flatpak must be enabled on the host (`services.flatpak.enable = true` on
NixOS), then:

```bash
flatpak remote-add --if-not-exists --user flathub https://dl.flathub.org/repo/flathub.flatpakrepo
flatpak install -y --user flathub org.flatpak.Builder
```

## Build and install

The manifest builds from `main`, not from the working tree, so commit first.

```bash
flatpak run --command=flathub-build org.flatpak.Builder --install \
    packaging/flatpak/page.codeberg.mmc.Spectral.yaml
flatpak run page.codeberg.mmc.Spectral
```

Flatpak exports one command per application and names it after the app ID,
so the CLI is on `$PATH` as `page.codeberg.mmc.Spectral`, never as
`spectral`. `flatpak build-finish` chooses which command runs (`--command=`)
but cannot name the exported wrapper, and Flathub requires exported files to
carry the app ID anyway — a short name is a shell alias on the user's side.

```bash
page.codeberg.mmc.Spectral status
```

To reach the daemon binary, which is not the exported command:

```bash
flatpak run --command=spectral-daemon page.codeberg.mmc.Spectral
```

**Stop any host daemon first.** The sandbox gets a private
`$XDG_RUNTIME_DIR`, so the Flatpak daemon and a natively installed one never
see each other's socket — but they will both try to publish a virtual mic
under the same node name, which makes any test result ambiguous.

## Lint

Both must be clean before submission:

```bash
flatpak run --command=flatpak-builder-lint org.flatpak.Builder manifest \
    packaging/flatpak/page.codeberg.mmc.Spectral.yaml
flatpak run --command=flatpak-builder-lint org.flatpak.Builder repo repo
```

## Regenerating cargo-sources.json

Flathub builds with no network, so every crate is listed as an explicit
source with a checksum. The script fetches everything it needs on demand and
leaves nothing behind, so the dev shell carries no tooling for a target it
never builds. Whenever `Cargo.lock` changes:

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

Deliberately absent: `--device=dri` (`src/main.rs` forces Slint's software
renderer, so there is no GPU path to grant), `--socket=pulseaudio` (Spectral
speaks native PipeWire and never touches the compatibility socket), network
access, home access, and any D-Bus name.

Note that `src/platform/linux/mod.rs::manager_client_properties` is what makes
the sandboxed build work at all: WirePlumber grants an undeclared Flatpak
client only `rx`, which cannot create the virtual mic or move the default
input.

## Differences from the native builds

- **No start at login.** The Nix module installs a systemd user service; the
  Flatpak has no equivalent. The daemon outlives the GUI window, so the
  virtual mic persists for the session, but not across logins.
- **The CLI answers to the app ID.** Flatpak exports a single command per
  application, named after the app ID, so it is `page.codeberg.mmc.Spectral`
  on `$PATH` rather than `spectral`.
- **Paths move under `~/.var/app/page.codeberg.mmc.Spectral/`.** Settings and
  voice-test recordings live there rather than under `~/.config` and
  `~/.local/state`.

## For Flathub

The submission repository holds this manifest with one change — the local
source is replaced by the published release:

```yaml
    sources:
      - type: git
        url: https://codeberg.org/mmc/spectral.git
        tag: vX.Y.Z
        commit: <the full sha that tag points at>
      - cargo-sources.json
```

A pinned `commit` alongside the `tag` is deliberate: Forgejo's generated
archives are not guaranteed to stay byte-identical, so an `archive` source
with a `sha256` is the more fragile choice here.

`cargo-sources.json` is copied across unchanged. Everything else — the app ID,
the runtime, the permissions, both modules — is identical.

The submission process itself is documented at
<https://docs.flathub.org/docs/for-app-authors/submission>. Read the
[requirements](https://docs.flathub.org/docs/for-app-authors/requirements)
first, in particular the generative AI policy.
