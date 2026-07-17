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
            buildInputs = [
              rustToolchain
              pkgs.pkg-config
              pkgs.clang
              pkgs.gh
              pkgs.pacman
              pkgs.appstream
              pkgs.curl
            ]
            ++ spectral.passthru.runtimeLibs;

            LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
            MAKEPKG_CONF = "${pkgs.pacman}/etc/makepkg.conf";
            LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath spectral.passthru.runtimeLibs;
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
              # Deliberately no install hooks or checks: the package's
              # postInstall and versionCheckHook expect binaries that a
              # lint-only derivation never produces.
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
              # Same rationale as the clippy check: a test-only derivation
              # produces no binaries for install hooks to verify.
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
