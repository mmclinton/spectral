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

        spectral = pkgs.rustPlatform.buildRustPackage {
          inherit pname version;

          src = ./.;

          cargoLock = {
            lockFile = ./Cargo.lock;
          };

          # Environment variables for build
          LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";

          # Native build inputs (e.g., for C compilers or pkg-config)
          nativeBuildInputs = with pkgs; [
            pkg-config
            cmake
            python3
            clang
          ];

          # Runtime/build dependencies (e.g., for linking libraries)
          buildInputs = with pkgs; [
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

            # Audio dependencies for PipeWire
            pipewire
            glib
            dbus
            systemd
            pulseaudio

            # Additional libraries
            libclang.lib
          ];
          postInstall = pkgs.lib.optionalString (pname != binaryName) ''
            mv $out/bin/${pname} $out/bin/${binaryName}
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
          buildInputs = with pkgs; [
            rustToolchain
            pkg-config
            cmake
            python3
            clang

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

            # Audio dependencies for PipeWire
            pipewire
            pipewire.dev

            # System libraries
            glib
            dbus
            systemd
            pulseaudio
          ];
          NIX_ENFORCE_PURITY = "0";
          RUST_BACKTRACE = "1";

          # Environment variables for build tools
          LIBCLANG_PATH = "${pkgs.libclang.lib}/lib";
          LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath [
            pkgs.libGL
            pkgs.libxkbcommon
            pkgs.wayland
            pkgs.xorg.libX11
            pkgs.xorg.libXcursor
            pkgs.xorg.libXi
            pkgs.xorg.libXrandr
            pkgs.fontconfig
            pkgs.freetype
            pkgs.pipewire
            pkgs.glib
            pkgs.dbus
            pkgs.systemd
            pkgs.pulseaudio
          ];

          shellHook = '''';
        };

        # Apps: Run with `nix run`
        apps.default = {
          type = "app";
          program = "${spectral}/bin/${binaryName}";
        };
      }
    );
}
