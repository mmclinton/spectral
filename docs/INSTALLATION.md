# Installation

Spectral is in nixpkgs as `spectral-mic`. The AUR submission is still pending, and a Flatpak manifest is included and builds cleanly but has not been through a Flathub review. Manual builds may work but are unsupported.

Spectral needs Linux with **PipeWire** and its session manager, **WirePlumber**, on Wayland or X11, x86_64 or aarch64.

- [NixOS / Nix](#nixos--nix)
- [Arch Linux (AUR)](#arch-linux-aur)
- [Flatpak](#flatpak)

## NixOS / Nix

Spectral is in nixpkgs as `spectral-mic` — the attribute carries the suffix, the binary it installs is `spectral`. Whether your channel has it yet depends on when that channel branched; `nix search nixpkgs spectral-mic` answers that for the one you are on.

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
nix run git+https://codeberg.org/mmc/spectral

# Or build and install to your profile
nix profile add git+https://codeberg.org/mmc/spectral
```

### NixOS, declaratively

Add to your `flake.nix` inputs:

```nix
{
  inputs = {
    spectral.url = "git+https://codeberg.org/mmc/spectral";
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

## Arch Linux (AUR)

**Spectral is not on the AUR yet.** Submission of `spectral-mic` is pending; once it lands, this becomes the install command:

```bash
paru -S spectral-mic   # or: yay -S spectral-mic — not available yet
```

Until then, build from the PKGBUILD in this repository:

```bash
git clone https://codeberg.org/mmc/spectral.git
cd spectral/packaging/aur
makepkg -si
```

The PKGBUILD fetches and builds the release tarball its `pkgver` names, not the checkout it sits in, and declares x86_64 only — no aarch64 Arch system has been tested.

The package is named `spectral-mic` rather than `spectral` because the unrelated AUR package of that name ships a binary called `spectral` too. The PKGBUILD is maintained here under [packaging/aur](../packaging/aur).

## Flatpak

**Spectral is not on Flathub yet.** Until it is, build the Flatpak yourself — it needs only `flatpak` and the Flathub remote:

```bash
flatpak install -y flathub org.flatpak.Builder
git clone https://codeberg.org/mmc/spectral.git
cd spectral
flatpak run --command=flathub-build org.flatpak.Builder --install \
    packaging/flatpak/page.codeberg.mmc.Spectral.yaml
flatpak run page.codeberg.mmc.Spectral
```

One difference from the native packages is worth knowing before you choose this route: Flatpak exports a single command per application, named after the app ID, so the [headless CLI](CLI.md) answers to `page.codeberg.mmc.Spectral` rather than to `spectral`:

```bash
page.codeberg.mmc.Spectral devices
```

Alias it if the full name grates — Flatpak has no way to export a shorter one.

Details, including the full permission set and what a Flathub submission would change, are in [packaging/flatpak](../packaging/flatpak).
