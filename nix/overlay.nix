# Nix overlay for Spectral
#
# Example usage in flake.nix:
#   nixpkgs.overlays = [ inputs.spectral.overlays.default ];
#
# Then use: pkgs.spectral
{ crane, rust-overlay }:
let
  spectralFor = import ./spectral-for.nix { inherit crane rust-overlay; };
in
final: prev: {
  spectral = spectralFor final;
}
