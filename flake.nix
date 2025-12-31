{
  description = "Spectral - Real-time audio noise suppression application.";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    {
      self,
      nixpkgs,
      rust-overlay,
      flake-utils,
      ...
    }:
    let
      # System-independent outputs
      systemIndependentOutputs = {
        # Overlay for adding spectral to pkgs
        overlays.default = import ./nix/overlay.nix;

        # NixOS module for system-wide installation
        nixosModules.default = import ./nix/module.nix;
        nixosModules.spectral = import ./nix/module.nix;
      };

      # Per-system outputs
      perSystemOutputs = flake-utils.lib.eachDefaultSystem (
        system:
        let
          pkgs = import nixpkgs { inherit system; };

          # Package built from nix/package.nix
          spectral = pkgs.callPackage ./nix/package.nix { };

          # Development toolchain with rust-overlay
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

          # Runtime libraries needed for development
          runtimeLibs = with pkgs; [
            libGL
            libxkbcommon
            wayland
            xorg.libX11
            xorg.libXcursor
            xorg.libXi
            xorg.libXrandr
            fontconfig
            freetype
            pipewire
            libpulseaudio
          ];
        in
        {
          packages.default = spectral;
          packages.spectral = spectral;

          devShells.default = pkgs.mkShell {
            buildInputs = [
              rustToolchain
              pkgs.pkg-config
              pkgs.clang
            ]
            ++ runtimeLibs;

            LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
            LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath runtimeLibs;
            RUST_BACKTRACE = "1";
          };

          # Apps: Run with `nix run`
          apps.default = {
            type = "app";
            program = "${spectral}/bin/spectral";
          };

          apps.daemon = {
            type = "app";
            program = "${spectral}/bin/spectral-daemon";
          };
        }
      );
    in
    systemIndependentOutputs // perSystemOutputs;
}
