# Package definition for Spectral, called by the flake's packages and by
# spectral-for.nix.
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
  version = "0.11.0";

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

  # Only what the compiler reads, which includes the xtask crate: Cargo
  # cannot load the workspace without the member's manifest, and the
  # install phase compiles and runs it. A docs or CI commit must not change
  # the derivation hash and force a recompile.
  buildFiles = lib.fileset.unions [
    ./../src
    ./../ui
    ./../xtask
    ./../build.rs
    ./../Cargo.toml
    ./../Cargo.lock
    ./../.cargo/config.toml
  ];

  # Shipped verbatim by the install phase, or read by the tests, or both —
  # but never opened by rustfmt, which parses .rs files alone and expands no
  # macros. Keeping them out of buildFiles is what stops a typo fix in the
  # man page from re-running the format check; everything else here
  # consumes them.
  dataFiles = lib.fileset.unions [
    # The whole directory, so a new asset reaches the install phase and
    # xtask's asset test without an edit here. Neither the build nor the
    # install reads the screenshot; leaving it out keeps replacing it from
    # recompiling the crate.
    (lib.fileset.difference ./../assets ./../assets/screenshot.png)
    ./../docs/CLI.md
    ./../LICENSE
    ./../THIRD-PARTY.md
  ];

  src = lib.fileset.toSource {
    root = ./..;
    fileset = lib.fileset.union buildFiles dataFiles;
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

    # Every installed file comes from the one list in xtask/src/install.rs.
    installPhaseCommand = ''
      cargo xtask install --prefix "$out"
    '';

    postInstall = ''
      # The GUI shells out to xdg-open for its Help links; hosts without
      # xdg-utils would otherwise fail the spawn silently.
      wrapProgram $out/bin/spectral \
        --prefix LD_LIBRARY_PATH : "${lib.makeLibraryPath dlopenLibs}" \
        --suffix PATH : "${lib.makeBinPath [ xdg-utils ]}"
    '';

    # Fails the build when the binary's version disagrees with `version`
    # above — catches a missed spot in the release bump, whose full list of
    # spots packaging/bump-version.sh owns.
    nativeInstallCheckInputs = [ versionCheckHook ];
    doInstallCheck = true;

    passthru = {
      inherit linkedLibs dlopenLibs cargoArtifacts;

      tests = {
        clippy = craneLib'.cargoClippy (
          commonArgs
          // {
            inherit cargoArtifacts;
            cargoClippyExtraArgs = "--workspace --all-targets -- --deny warnings";
            # Same reason rustfmt.toml joins the fmt source below: clippy.toml
            # is not a build input, and putting it in buildFiles would make
            # every lint tweak recompile the crate. --all-targets compiles
            # the test target, so the data files have to be here.
            src = lib.fileset.toSource {
              root = ./..;
              fileset = lib.fileset.unions [
                buildFiles
                dataFiles
                ./../clippy.toml
              ];
            };
          }
        );

        unit = craneLib'.cargoTest (
          commonArgs
          // {
            inherit cargoArtifacts;
            cargoTestExtraArgs = "--workspace";
            # The PKGBUILD version test reads this at run time; only this
            # check needs it, so a recipe edit reruns the tests alone.
            src = lib.fileset.toSource {
              root = ./..;
              fileset = lib.fileset.unions [
                buildFiles
                dataFiles
                ./../packaging/arch/PKGBUILD
              ];
            };
          }
        );

        fmt = craneLib'.cargoFmt {
          inherit pname version;
          cargoExtraArgs = "--all";
          # rustfmt.toml is not a build input, so it joins the source only
          # here. dataFiles is deliberately absent: this is the one check
          # that reads none of it.
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
      homepage = "https://github.com/mmclinton/spectral";
      license = licenses.gpl3Only;
      maintainers = [
        {
          name = "mmc";
          email = "github.mmc@pm.me";
        }
      ];
      mainProgram = "spectral";
      platforms = platforms.linux;
    };
  }
)
