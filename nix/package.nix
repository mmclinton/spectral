# Standalone package definition for Spectral
# Can be used with callPackage or imported directly
{
  lib,
  stdenv,
  rustPlatform,
  pkg-config,
  clang,
  makeWrapper,
  makeFontsConf,
  libGL,
  libxkbcommon,
  wayland,
  xorg,
  fontconfig,
  freetype,
  pipewire,
  libpulseaudio,
  llvmPackages,
  copyDesktopItems,
  makeDesktopItem,
  jetbrains-mono,
}:
let
  pname = "spectral";
  version = "0.1.0";

  runtimeLibs = [
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
    libpulseaudio
  ];
in
rustPlatform.buildRustPackage {
  inherit pname version;

  src = ./..;

  cargoLock = {
    lockFile = ./../Cargo.lock;
  };

  LIBCLANG_PATH = "${llvmPackages.libclang.lib}/lib";

  nativeBuildInputs = [
    pkg-config
    clang
    makeWrapper
    copyDesktopItems
  ];

  buildInputs = runtimeLibs;

  desktopItems = [
    (makeDesktopItem {
      name = "spectral";
      desktopName = "Spectral";
      comment = "Real-time audio noise suppression";
      exec = "spectral";
      icon = "spectral";
      terminal = false;
      type = "Application";
      categories = [
        "AudioVideo"
        "Audio"
        "Mixer"
      ];
      keywords = [
        "noise"
        "suppression"
        "microphone"
        "audio"
        "rnnoise"
      ];
      startupNotify = true;
      startupWMClass = "spectral";
    })
  ];

  postInstall = ''
    # Install icon
    install -Dm644 assets/spectral.png $out/share/icons/hicolor/512x512/apps/spectral.png

    # Wrap binaries with runtime library paths and fallback font
    # Use winit backend (not Qt) to ensure set_xdg_app_id works for dock icon
    wrapProgram $out/bin/spectral \
      --prefix LD_LIBRARY_PATH : "${lib.makeLibraryPath runtimeLibs}" \
      --set FONTCONFIG_FILE "${makeFontsConf { fontDirectories = [ jetbrains-mono ]; }}" \
      --set SLINT_BACKEND "winit"

    wrapProgram $out/bin/spectral-daemon \
      --prefix LD_LIBRARY_PATH : "${lib.makeLibraryPath runtimeLibs}"
  '';

  meta = with lib; {
    description = "Real-time audio noise suppression application";
    longDescription = ''
      Spectral is a real-time audio noise suppression application powered by 
      RNNoise, a hybrid DSP and neural-network-based noise suppression system.
      It creates a virtual microphone that can be used with any application,
      filtering out background noise in real-time.
    '';
    homepage = "https://codeberg.org/mmc/spectral";
    license = licenses.gpl3Only;
    maintainers = [
      {
        name = "mmc";
        email = "codeberg.wilder850@simplelogin.com";
      }
    ];
    mainProgram = "spectral";
    platforms = platforms.linux;
  };
}
