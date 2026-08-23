# Nix overlay for Spectral
#
# Example usage in flake.nix:
#   nixpkgs.overlays = [ inputs.spectral.overlays.default ];
#
# Then use: pkgs.spectral
{ crane, rust-overlay }:
final: prev: {
  spectral = final.callPackage ./package.nix {
    craneLib = crane.mkLib final;
    rustToolchain = (final.appendOverlays [ (import rust-overlay) ]).rust-bin.stable.latest.default;
  };
}
