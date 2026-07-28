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
      systemIndependentOutputs = {
        overlays.default = import ./nix/overlay.nix;

        nixosModules.default = import ./nix/module.nix;
        nixosModules.spectral = import ./nix/module.nix;
      };

      perSystemOutputs = flake-utils.lib.eachDefaultSystem (
        system:
        let
          pkgs = import nixpkgs { inherit system; };

          spectral = pkgs.callPackage ./nix/package.nix { };

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
              pkgs.cargo-deny

              # Runs flatpak-cargo-generator.py, which turns Cargo.lock into
              # the offline source list Flathub's network-less builder needs.
              (pkgs.python3.withPackages (ps: [
                ps.aiohttp
                ps.tomlkit
                ps.pyyaml
              ]))
            ]
            ++ spectral.passthru.linkedLibs;

            MAKEPKG_CONF = "${pkgs.pacman}/etc/makepkg.conf";
            LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath spectral.passthru.dlopenLibs;
            RUST_BACKTRACE = "1";
          };

          checks = {
            build = spectral;

            clippy = spectral.overrideAttrs (old: {
              pname = "spectral-clippy";
              nativeBuildInputs = old.nativeBuildInputs ++ [ pkgs.clippy ];
              buildPhase = ''
                runHook preBuild
                cargo clippy --all-targets --locked -- --deny warnings
                runHook postBuild
              '';
              doCheck = false;
              doInstallCheck = false;
              installPhase = "touch $out";
            });

            tests = spectral.overrideAttrs (old: {
              pname = "spectral-tests";
              buildPhase = ''
                runHook preBuild
                cargo test --locked
                runHook postBuild
              '';
              doCheck = false;
              doInstallCheck = false;
              installPhase = "touch $out";
            });

            fmt =
              pkgs.runCommand "spectral-fmt"
                {
                  nativeBuildInputs = [
                    pkgs.cargo
                    pkgs.rustfmt
                  ];
                }
                ''
                  cd ${self}
                  export CARGO_HOME="$TMPDIR/cargo"
                  cargo fmt --check
                  touch $out
                '';
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
