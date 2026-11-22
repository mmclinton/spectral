# Spectral built against a given package set: the overlay's package, and the
# NixOS module's when the overlay is not applied.
{ crane, rust-overlay }:
pkgs:
pkgs.callPackage ./package.nix {
  craneLib = crane.mkLib pkgs;
  rustToolchain = (pkgs.appendOverlays [ (import rust-overlay) ]).rust-bin.stable.latest.default;
}
