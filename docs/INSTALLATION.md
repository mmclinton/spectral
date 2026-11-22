# Installation

Spectral installs through Nix. Before installing, check the [requirements](../README.md#requirements).

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

## Other formats

No other package is published. Developers can build the Flatpak, Arch and deb packages from the recipes in [packaging/](../packaging/README.md); any other manual build may work but is unsupported. Anyone is welcome to package the release tags; on the AUR, note that the name `spectral` is taken by an unrelated tool.
