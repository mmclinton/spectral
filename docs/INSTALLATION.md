# Installation

Spectral installs through Nix. Before installing, check the [requirements](../README.md#requirements).

## Nix

The flake in this repository is the recommended route, and the only one with a NixOS module. It tracks `main`, not the last release:

```bash
nix run github:mmclinton/spectral
```

On NixOS, add `inputs.spectral.url = "github:mmclinton/spectral";` to your flake, then:

```nix
{ inputs, ... }:
{
  imports = [ inputs.spectral.nixosModules.default ];
  programs.spectral.enable = true;
}
```

Between releases the version stays the same, so a daemon still running from before an update keeps the old build: run `spectral shutdown` after updating, and the next launch starts the new one.

Spectral is also in nixpkgs as `spectral-mic`, which installs the `spectral` command; [search.nixos.org](https://search.nixos.org/packages?query=spectral-mic) shows which channels have it.

## Other formats

No other package is published. Developers can build the Flatpak, Arch and deb packages from the recipes in [packaging/](../packaging/README.md); any other manual build may work but is unsupported. Anyone is welcome to package the release tags; on the AUR, note that the name `spectral` is taken by an unrelated tool.
