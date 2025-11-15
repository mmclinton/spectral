{ pkgs ? import <nixpkgs> {} }:

let
  libPath = with pkgs; lib.makeLibraryPath [
    fontconfig
    libGL
    libxkbcommon
    wayland
    xorg.libX11
    xorg.libXcursor
    xorg.libXi
    xorg.libXrandr
  ];
in

pkgs.mkShell {
  nativeBuildInputs = with pkgs; [
    cargo
    clippy
    pkg-config
    rust-analyzer
    rustc
    rustfmt
    slint-lsp
    slint-viewer
  ];

  buildInputs = with pkgs; [
    openssl
  ];

  LD_LIBRARY_PATH = libPath;
}
