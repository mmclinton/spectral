# Standalone package definition for Spectral
# Can be used with callPackage or imported directly
{
  lib,
  rustPlatform,
  pkg-config,
  clang,
  llvmPackages,
  makeWrapper,
  makeFontsConf,
  libxkbcommon,
  wayland,
  libx11,
  libxcursor,
  libxi,
  libxrandr,
  fontconfig,
  freetype,
  pipewire,
  jetbrains-mono,
  versionCheckHook,
}:
let
  pname = "spectral";
  version = "0.5.0";

  runtimeLibs = [
    libxkbcommon
    wayland
    libx11
    libxcursor
    libxi
    libxrandr
    fontconfig
    freetype
    pipewire
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
    clang
    makeWrapper
  ];

  buildInputs = runtimeLibs;

  passthru = { inherit runtimeLibs; };

  env.LIBCLANG_PATH = "${llvmPackages.libclang.lib}/lib";

  postInstall = ''
    install -Dm644 assets/page.codeberg.mmc.Spectral.desktop $out/share/applications/page.codeberg.mmc.Spectral.desktop
    install -Dm644 assets/page.codeberg.mmc.Spectral.metainfo.xml $out/share/metainfo/page.codeberg.mmc.Spectral.metainfo.xml
    install -Dm644 assets/spectral.png $out/share/icons/hicolor/512x512/apps/page.codeberg.mmc.Spectral.png
    install -Dm644 THIRD-PARTY.md $out/share/licenses/spectral/THIRD-PARTY.md

    wrapProgram $out/bin/spectral \
      --prefix LD_LIBRARY_PATH : "${lib.makeLibraryPath runtimeLibs}" \
      --set FONTCONFIG_FILE "${makeFontsConf { fontDirectories = [ jetbrains-mono ]; }}"

    wrapProgram $out/bin/spectral-daemon \
      --prefix LD_LIBRARY_PATH : "${lib.makeLibraryPath runtimeLibs}"
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
