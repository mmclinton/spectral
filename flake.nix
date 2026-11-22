{
  description = "Spectral - Real-time audio noise suppression application.";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    rust-overlay.inputs.nixpkgs.follows = "nixpkgs";
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
      overlay = import ./nix/overlay.nix { inherit crane rust-overlay; };
      spectralFor = import ./nix/spectral-for.nix { inherit crane rust-overlay; };

      nixosModule = {
        # Lets the module system import it once however often it is listed.
        key = "github:mmclinton/spectral#nixosModules.default";
        _file = ./flake.nix;
        imports = [
          ./nix/module.nix
          (
            { lib, pkgs, ... }:
            {
              # Without this flake's overlay, `pkgs.spectral` is nixpkgs'
              # unrelated Spectral typeface. One that is this app, the
              # overlay's or an override of it, is kept.
              programs.spectral.package = lib.mkDefault (
                let
                  current = pkgs.spectral or null;
                in
                if current != null && (current.meta.mainProgram or null) == "spectral" then
                  current
                else
                  spectralFor pkgs
              );
            }
          )
        ];
      };

      systemIndependentOutputs = {
        overlays.default = overlay;

        nixosModules.default = nixosModule;
        nixosModules.spectral = nixosModule;
      };

      perSystemOutputs = flake-utils.lib.eachSystem [ "x86_64-linux" "aarch64-linux" ] (
        system:
        let
          pkgs = import nixpkgs { inherit system; };

          rustPkgs = import nixpkgs {
            inherit system;
            overlays = [ (import rust-overlay) ];
          };

          rustToolchain = rustPkgs.rust-bin.stable.latest.default;

          spectral = pkgs.callPackage ./nix/package.nix {
            craneLib = crane.mkLib pkgs;
            inherit rustToolchain;
          };

        in
        {
          packages.default = spectral;
          packages.spectral = spectral;

          devShells.default = pkgs.mkShell {
            nativeBuildInputs = [ pkgs.rustPlatform.bindgenHook ];

            buildInputs = [
              (rustToolchain.override {
                extensions = [
                  "rust-analyzer"
                  "rust-src"
                ];
              })
              pkgs.pkg-config
              pkgs.gh
              pkgs.appstream
              pkgs.curl
              pkgs.jq
              pkgs.cargo-deny
            ]
            ++ spectral.passthru.linkedLibs;

            LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath spectral.passthru.dlopenLibs;
            RUST_BACKTRACE = "1";
          };

          checks = {
            build = spectral;
            inherit (spectral.passthru.tests) clippy unit fmt;

            # Evaluation only. Without the overlay, `pkgs.spectral` is nixpkgs'
            # typeface, so the module must install a package that is this app;
            # with it, an override of `pkgs.spectral` must reach the module.
            nixos-module =
              let
                configWith =
                  extra:
                  (nixpkgs.lib.nixosSystem {
                    modules = [
                      self.nixosModules.default
                      {
                        nixpkgs.hostPlatform = system;
                        programs.spectral.enable = true;
                        boot.loader.grub.enable = false;
                        fileSystems."/" = {
                          device = "none";
                          fsType = "tmpfs";
                        };
                        system.stateVersion = "25.11";
                      }
                      extra
                    ];
                  }).config;
                bare = configWith { };
                overridden = configWith {
                  nixpkgs.overlays = [
                    self.overlays.default
                    (final: prev: {
                      spectral = prev.spectral.overrideAttrs { pname = "spectral-overridden"; };
                    })
                  ];
                };
                package = bare.programs.spectral.package;
              in
              assert (package.meta.mainProgram or null) == "spectral";
              assert builtins.elem package bare.environment.systemPackages;
              assert overridden.programs.spectral.package.pname == "spectral-overridden";
              pkgs.runCommand "nixos-module-eval" { } "touch $out";
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
