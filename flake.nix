{
  description = "Spectral - Real-time audio noise suppression application.";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    flake-utils.url = "github:numtide/flake-utils";
    crane.url = "github:ipetkov/crane";
  };

  outputs =
    {
      self,
      nixpkgs,
      rust-overlay,
      flake-utils,
      crane,
      ...
    }:
    let
      systemIndependentOutputs = {
        overlays.default = import ./nix/overlay.nix { inherit crane; };

        nixosModules.default = import ./nix/module.nix;
        nixosModules.spectral = import ./nix/module.nix;
      };

      perSystemOutputs = flake-utils.lib.eachDefaultSystem (
        system:
        let
          pkgs = import nixpkgs { inherit system; };

          spectral = pkgs.callPackage ./nix/package.nix { craneLib = crane.mkLib pkgs; };

          rustPkgs = import nixpkgs {
            inherit system;
            overlays = [ (import rust-overlay) ];
          };

          rustToolchain = rustPkgs.rust-bin.stable.latest.default.override {
            extensions = [
              "rust-analyzer"
              "rust-src"
              "rustfmt"
              "clippy"
            ];
          };

        in
        {
          packages.default = spectral;
          packages.spectral = spectral;

          devShells.default = pkgs.mkShell {
            nativeBuildInputs = [ pkgs.rustPlatform.bindgenHook ];

            buildInputs = [
              rustToolchain
              pkgs.pkg-config
              pkgs.gh
              pkgs.pacman
              pkgs.appstream
              pkgs.curl
              pkgs.jq
              pkgs.cargo-deny
            ]
            ++ spectral.passthru.linkedLibs;

            MAKEPKG_CONF = "${pkgs.pacman}/etc/makepkg.conf";
            LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath spectral.passthru.dlopenLibs;
            RUST_BACKTRACE = "1";
          };

          checks = {
            build = spectral;
            inherit (spectral.passthru.tests) clippy unit fmt;
          };

          apps.default = {
            type = "app";
            program = "${spectral}/bin/spectral";
            meta.description = "Spectral GUI control panel";
          };

          apps.daemon = {
            type = "app";
            program = "${spectral}/bin/spectral-daemon";
            meta.description = "Spectral audio-processing daemon";
          };
        }
      );
    in
    systemIndependentOutputs // perSystemOutputs;
}
