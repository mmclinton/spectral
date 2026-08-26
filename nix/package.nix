# Package definition for Spectral, used by the flake and the overlay.
#
# Built with crane so the dependency artifacts are a derivation of their
# own: a source change recompiles only this crate, and the lint and test
# checks in passthru.tests start from the same artifacts instead of each
# rebuilding every dependency from scratch.
{
  lib,
  craneLib,
  rustToolchain,
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
  xdg-utils,
  versionCheckHook,
}:
let
  # rust-overlay ships upstream's optimized rustc, which compiles this crate
  # in 28 s where nixpkgs's from-source build takes 48. It is also the dev
  # shell's toolchain, so the checks lint and format with the same versions.
  craneLib' = craneLib.overrideToolchain rustToolchain;

  pname = "spectral";
  version = "0.9.0";

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

  # Only what the build reads. A docs, packaging, or CI commit must not
  # change the derivation hash and force a recompile.
  buildFiles = lib.fileset.unions [
    ./../src
    ./../ui
    ./../build.rs
    ./../Cargo.toml
    ./../Cargo.lock
    ./../assets/page.codeberg.mmc.Spectral.desktop
    ./../assets/page.codeberg.mmc.Spectral.metainfo.xml
    ./../assets/spectral.png
    ./../THIRD-PARTY.md
    # The CLI tests include_str! this.
    ./../docs/CLI.md
  ];

  src = lib.fileset.toSource {
    root = ./..;
    fileset = buildFiles;
  };

  commonArgs = {
    inherit pname version src;

    __structuredAttrs = true;
    strictDeps = true;

    nativeBuildInputs = [
      pkg-config
      rustPlatform.bindgenHook
    ];

    buildInputs = linkedLibs;

    # stdenv derives -frandom-seed from $out and bindgenHook copies it into
    # BINDGEN_EXTRA_CLANG_ARGS, which bindgen's build script watches; a
    # per-derivation seed would make every consumer of the artifacts
    # recompile bindgen and the pipewire -sys crates. One fixed seed for all
    # of them — crane's documented fix (https://crane.dev/faq/rebuilds-bindgen.html).
    env.NIX_OUTPATH_USED_AS_RANDOM_SEED = "spectral00";
  };

  cargoArtifacts = craneLib'.buildDepsOnly commonArgs;
in
craneLib'.buildPackage (
  commonArgs
  // {
    inherit cargoArtifacts;

    nativeBuildInputs = commonArgs.nativeBuildInputs ++ [ makeWrapper ];

    # The tests gate every PR through passthru.tests.unit; running them
    # here as well would rebuild the test targets on every install.
    doCheck = false;

    postInstall = ''
      install -Dm644 assets/page.codeberg.mmc.Spectral.desktop $out/share/applications/page.codeberg.mmc.Spectral.desktop
      install -Dm644 assets/page.codeberg.mmc.Spectral.metainfo.xml $out/share/metainfo/page.codeberg.mmc.Spectral.metainfo.xml
      install -Dm644 assets/spectral.png $out/share/icons/hicolor/512x512/apps/page.codeberg.mmc.Spectral.png
      install -Dm644 THIRD-PARTY.md $out/share/licenses/spectral/THIRD-PARTY.md

      # The GUI shells out to xdg-open for its Help links; hosts without
      # xdg-utils would otherwise fail the spawn silently.
      wrapProgram $out/bin/spectral \
        --prefix LD_LIBRARY_PATH : "${lib.makeLibraryPath dlopenLibs}" \
        --prefix PATH : "${lib.makeBinPath [ xdg-utils ]}"
    '';

    # Fails the build when the binary's version disagrees with `version`
    # above — catches a missed spot in the 3-place release bump.
    nativeInstallCheckInputs = [ versionCheckHook ];
    doInstallCheck = true;

    passthru = {
      inherit linkedLibs dlopenLibs cargoArtifacts;

      tests = {
        clippy = craneLib'.cargoClippy (
          commonArgs
          // {
            inherit cargoArtifacts;
            cargoClippyExtraArgs = "--all-targets -- --deny warnings";
          }
        );

        unit = craneLib'.cargoTest (commonArgs // { inherit cargoArtifacts; });

        fmt = craneLib'.cargoFmt {
          inherit pname version;
          # rustfmt.toml is not a build input, so it joins the source only
          # here.
          src = lib.fileset.toSource {
            root = ./..;
            fileset = lib.fileset.union buildFiles ./../rustfmt.toml;
          };
        };
      };
    };

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
)
