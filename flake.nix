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
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };

        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [
            "rust-analyzer"
            "rust-src"
            "rustfmt"
            "clippy"
          ];
        };

        pname = "spectral";
        version = "0.1.0";
        binaryName = "spectral";

        # Runtime libraries that need to be dynamically linked
        runtimeLibs = with pkgs; [
          # GUI/Graphics dependencies for Slint
          libGL
          libxkbcommon
          wayland
          xorg.libX11
          xorg.libXcursor
          xorg.libXi
          xorg.libXrandr
          fontconfig
          freetype

          # Audio dependencies
          pipewire
          libpulseaudio # Provides libpulse.so.0 for libpulse-binding
        ];

        # Runtime tools that need to be in the PATH
        runtimeTools = with pkgs; [
          pipewire # Provides 'pw-record' and 'pw-play'
          wireplumber # PipeWire session manager
          pulseaudio # Provides 'pacat' for audio streaming (TODO: migrate to pw-record/pw-play)
        ];

        spectral = pkgs.rustPlatform.buildRustPackage {
          inherit pname version;

          src = ./.;

          cargoLock = {
            lockFile = ./Cargo.lock;
          };

          # Environment variables for build
          LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";

          # Native build inputs (build-time tools)
          nativeBuildInputs = with pkgs; [
            pkg-config
            cmake
            clang
            makeWrapper
          ];

          # Build inputs (libraries to link against)
          buildInputs = runtimeLibs;

          postInstall = ''
            wrapProgram $out/bin/spectral \
              --prefix LD_LIBRARY_PATH : "${pkgs.lib.makeLibraryPath runtimeLibs}" \
              --prefix PATH : "${pkgs.lib.makeBinPath runtimeTools}"

            wrapProgram $out/bin/spectral-daemon \
              --prefix LD_LIBRARY_PATH : "${pkgs.lib.makeLibraryPath runtimeLibs}" \
              --prefix PATH : "${pkgs.lib.makeBinPath runtimeTools}"
          '';

          meta = with pkgs.lib; {
            description = "Real-time audio noise suppression application";
            homepage = "https://codeberg.org/spectral";
            license = licenses.mit;
            maintainers = [ ];
            mainProgram = binaryName;
            platforms = platforms.linux;
          };
        };
      in
      {
        packages.default = spectral;
        packages.${pname} = spectral;

        devShells.default = pkgs.mkShell {
          buildInputs =
            with pkgs;
            [
              rustToolchain
              pkg-config
              cmake
              clang
            ]
            ++ runtimeLibs
            ++ runtimeTools;

          NIX_ENFORCE_PURITY = "0";
          RUST_BACKTRACE = "1";

          # Environment variables for build tools
          LIBCLANG_PATH = "${pkgs.libclang.lib}/lib";

          # Ensure libraries are found during cargo run
          LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath runtimeLibs;
        };

        # Apps: Run with `nix run`
        apps.default = {
          type = "app";
          program = "${spectral}/bin/${binaryName}";
        };
      }
    );
}
