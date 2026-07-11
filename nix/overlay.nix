# Nix overlay for Spectral
#
# Example usage in flake.nix:
#   nixpkgs.overlays = [ inputs.spectral.overlays.default ];
#
# Then use: pkgs.spectral
final: prev: {
  spectral = final.callPackage ./package.nix { };
}
