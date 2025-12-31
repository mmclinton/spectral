# Nix overlay for Spectral
# Use this to add Spectral to your pkgs
#
# Example usage in flake.nix:
#   nixpkgs.overlays = [ inputs.spectral.overlays.default ];
#
# Then use: pkgs.spectral
final: prev: {
  spectral = final.callPackage ./package.nix { };
}
