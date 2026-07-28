# Standalone package definition for Spectral
# Can be used with callPackage or imported directly
{
  lib,
  rustPlatform,
  pkg-config,
  makeWrapper,
  libxkbcommon,
  wayland,
  libx11,
  libxcursor,
  libxi,
  libxrandr,
  fontconfig,
  pipewire,
  versionCheckHook,
}:
let
  pname = "spectral";
  version = "0.7.0";

  # Linked at build time (DT_NEEDED) — resolved through the RUNPATH the
  # cc-wrapper writes, no environment help needed.
  linkedLibs = [
    fontconfig
    pipewire
  ];

  # Loaded via dlopen at runtime by winit (x11-dl, xkbcommon-dl) and
  # wayland-sys — invisible to the linker, so the GUI wrapper must put
  # them on LD_LIBRARY_PATH.
  dlopenLibs = [
    libxkbcommon
    wayland
    libx11
    libxcursor
    libxi
    libxrandr
  ];
in
rustPlatform.buildRustPackage {
  inherit pname version;

  __structuredAttrs = true;

  src = ./..;

  cargoLock = {
    lockFile = ./../Cargo.lock;
  };

  nativeBuildInputs = [
    pkg-config
    rustPlatform.bindgenHook
    makeWrapper
  ];

  buildInputs = linkedLibs;

  passthru = { inherit linkedLibs dlopenLibs; };

  postInstall = ''
    install -Dm644 assets/page.codeberg.mmc.Spectral.desktop $out/share/applications/page.codeberg.mmc.Spectral.desktop
    install -Dm644 assets/page.codeberg.mmc.Spectral.metainfo.xml $out/share/metainfo/page.codeberg.mmc.Spectral.metainfo.xml
    install -Dm644 assets/spectral.png $out/share/icons/hicolor/512x512/apps/page.codeberg.mmc.Spectral.png
    install -Dm644 THIRD-PARTY.md $out/share/licenses/spectral/THIRD-PARTY.md

    wrapProgram $out/bin/spectral \
      --prefix LD_LIBRARY_PATH : "${lib.makeLibraryPath dlopenLibs}"
  '';

  # Fails the build when the binary's version disagrees with `version`
  # above — catches a missed spot in the 3-place release bump.
  nativeInstallCheckInputs = [ versionCheckHook ];
  doInstallCheck = true;

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
