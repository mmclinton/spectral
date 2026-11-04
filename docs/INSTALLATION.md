# Installation

Spectral is in nixpkgs as `spectral-mic`. A Flatpak manifest is included and builds cleanly; [Flatpak](#flatpak) below carries its publishing status. Manual builds may work but are unsupported.

Spectral needs Linux with **PipeWire** and its session manager, **WirePlumber**, on Wayland or X11, x86_64 or aarch64.

- [NixOS / Nix](#nixos--nix)
- [Flatpak](#flatpak)
- [What is deliberately not packaged](#what-is-deliberately-not-packaged)

## NixOS / Nix

Spectral is in nixpkgs as `spectral-mic` — the attribute carries the suffix, the binary it installs is `spectral`. Whether your channel has it yet depends on when that channel branched; [search.nixos.org](https://search.nixos.org/packages?query=spectral-mic) shows it per channel.

```nix
environment.systemPackages = [ pkgs.spectral-mic ];
```

```bash
# Or run it once, without installing anything
nix run nixpkgs#spectral-mic
```

The flake in this repository is the other Nix path, and it is not redundant: it tracks the latest commit rather than the last release, and it carries a NixOS module that nixpkgs does not ship. Note that its overlay provides `pkgs.spectral`, not `pkgs.spectral-mic`.

### Try it, or install into your profile

```bash
# Run directly without installing
nix run github:mmclinton/spectral

# Or build and install to your profile
nix profile add github:mmclinton/spectral
```

### NixOS, declaratively

Add to your `flake.nix` inputs:

```nix
{
  inputs = {
    spectral.url = "github:mmclinton/spectral";
  };
}
```

Then in your NixOS configuration:

```nix
{ inputs, ... }:
{
  imports = [ inputs.spectral.nixosModules.default ];

  # Apply the overlay to make pkgs.spectral available
  nixpkgs.overlays = [ inputs.spectral.overlays.default ];

  # Enable Spectral
  programs.spectral.enable = true;
}
```

### Overlay only

If you prefer manual control over installation:

```nix
{
  nixpkgs.overlays = [ inputs.spectral.overlays.default ];

  # Then use pkgs.spectral anywhere
  environment.systemPackages = [ pkgs.spectral ];
}
```

## Flatpak

**Spectral is not on Flathub yet.** Until it is, build the Flatpak yourself — it needs only `flatpak` and the Flathub remote:

```bash
flatpak install -y flathub org.flatpak.Builder
git clone https://github.com/mmclinton/spectral.git
cd spectral
flatpak run org.flatpak.Builder --user --install --install-deps-from=flathub \
    --force-clean builddir packaging/flatpak/io.github.mmclinton.Spectral.yaml
flatpak run io.github.mmclinton.Spectral
```

One difference from the native packages is worth knowing before you choose this route: Flatpak exports a single command per application, named after the app ID, so the [headless CLI](CLI.md) answers to `io.github.mmclinton.Spectral` rather than to `spectral`:

```bash
io.github.mmclinton.Spectral devices
```

Alias it if the full name grates — Flatpak has no way to export a shorter one.

Details, including the full permission set and what a Flathub submission would change, are in [packaging/flatpak](../packaging/flatpak).

## What is deliberately not packaged

There is no AUR package, and there are no deb or rpm packages, no PPA or COPR repositories, no AppImage, and no prebuilt binaries or release tarballs — none are planned. Flatpak already covers those desktops with a runtime this project can pin and an update cadence it controls, while Spectral moves too fast for distro archives, which would freeze it into stale packages. A prebuilt binary is the worst of the options: it inherits its build machine's loader and library paths and fails on everyone else's. If one of these formats matters to you, open an issue describing the need — the list is a decision, not an oversight. Anyone is welcome to package the release tags independently; on the AUR, note that the name `spectral` is taken by an unrelated tool.
