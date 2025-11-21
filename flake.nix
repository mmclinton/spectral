{
  description = "Spectral – cross-platform real-time noise suppression (NoiseTorch alternative)";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ rust-overlay.overlays.default ];
        };

        libPath = with pkgs; lib.makeLibraryPath [
          fontconfig libGL libxkbcommon wayland xorg.libX11
          xorg.libXcursor xorg.libXi xorg.libXrandr
          pipewire
        ];

        rust = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" "rust-analyzer" ];
        };
      in
      {
        devShells.default = pkgs.mkShell {
          nativeBuildInputs = with pkgs; [
            rust cargo clippy pkg-config rust-analyzer rustfmt
            slint-lsp slint-viewer
            pipewire.dev
            clang
            rust-bindgen
          ];

          buildInputs = with pkgs; [
            openssl
            pipewire
            glib dbus systemd
          ];

          LIBCLANG_PATH = "${pkgs.llvmPackages.clang-unwrapped.lib}/lib";
          LD_LIBRARY_PATH = libPath;
          hardeningDisable = [ "fortify" ];
        };

        packages.default = pkgs.stdenv.mkDerivation {
          pname = "spectral";
          version = "0.1.0";
          src = ./.;

          nativeBuildInputs = with pkgs; [
            rust pkg-config slint-build pipewire.dev clang rust-bindgen
          ];

          buildInputs = with pkgs; [
            openssl pipewire glib dbus systemd
            fontconfig libGL libxkbcommon wayland
            xorg.libX11 xorg.libXcursor xorg.libXi xorg.libXrandr
          ];

          LIBCLANG_PATH = "${pkgs.llvmPackages.clang-unwrapped.lib}/lib";

          buildPhase = "cargo build --release";
          installPhase = ''
            mkdir -p $out/bin
            cp target/release/spectral $out/bin/
          '';
        };

        apps.default = flake-utils.lib.mkApp { drv = self.packages.${system}.default; };
      });
}
