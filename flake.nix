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
    let
      systemIndependentOutputs = {
        overlays.default = import ./nix/overlay.nix;

        nixosModules.default = import ./nix/module.nix;
        nixosModules.spectral = import ./nix/module.nix;
      };

      perSystemOutputs = flake-utils.lib.eachDefaultSystem (
        system:
        let
          pkgs = import nixpkgs { inherit system; };

          spectral = pkgs.callPackage ./nix/package.nix { };

          rustPkgs = import nixpkgs {
            inherit system;
            overlays = [ (import rust-overlay) ];
          };

          rustToolchain = rustPkgs.rust-bin.stable.latest.default.override {
            extensions = [
              "rust-analyzer"
              "rust-src"
              "rustfmt"
              "clippy"
            ];
          };

          # Dev helper: cleanly stop any running spectral-daemon so a freshly
          # built one can take its place. SIGTERM triggers the daemon's
          # graceful path (virtual-mic teardown, state save, socket/pidfile
          # cleanup via Drop); SIGKILL is a last resort, after re-verifying
          # the PID still belongs to a spectral-daemon. The IPC Shutdown
          # command is avoided: it exits without running Drop, leaving stale
          # files — which are swept here instead. The virtual mic itself needs
          # no sweeping: its PipeWire objects die with the daemon process.
          # Process matching covers both dev builds (comm "spectral-daemon")
          # and Nix-wrapped installs (".spectral-daemon-wrapped",
          # comm-truncated to ".spectral-daemo"), with the pidfile as a
          # verified fallback for renamed wrappers.
          spectral-teardown = pkgs.writers.writeFishBin "spectral-teardown" ''
            set PATH ${pkgs.lib.makeBinPath (with pkgs; [ procps util-linux coreutils ])} $PATH

            set -l runtime_dir /tmp/runtime-$USER
            if set -q XDG_RUNTIME_DIR
                set runtime_dir $XDG_RUNTIME_DIR
            end
            set -l pidfile $runtime_dir/spectral-daemon.pid
            set -l socket $runtime_dir/spectral-daemon.sock

            function daemon_pids
                pgrep -u $USER -x 'spectral-daemon|\.spectral-daemo'
            end

            function is_daemon --argument-names pid
                test -r /proc/$pid/cmdline
                and string match -q -- '*spectral-daemon*' (string split0 < /proc/$pid/cmdline)
            end

            set -l pids (daemon_pids)

            if test -r $pidfile
                set -l fpid (string trim < $pidfile)
                if string match -qr '^[0-9]+$' -- $fpid
                    and not contains -- $fpid $pids
                    and is_daemon $fpid
                    set -a pids $fpid
                end
            end

            if test (count $pids) -eq 0
                echo "spectral-teardown: no running spectral-daemon"
            else
                echo "spectral-teardown: stopping spectral-daemon (pid $pids)"
                kill -TERM $pids
                set -l alive $pids
                for i in (seq 50)
                    set -l still
                    for p in $alive
                        test -e /proc/$p; and set -a still $p
                    end
                    set alive $still
                    if test (count $alive) -eq 0
                        break
                    end
                    sleep 0.1
                end
                if test (count $alive) -gt 0
                    echo "spectral-teardown: no clean exit after 5 s, sending SIGKILL" >&2
                    for p in $alive
                        is_daemon $p; and kill -KILL $p
                    end
                else
                    echo "spectral-teardown: daemon exited cleanly"
                end
            end

            if test (count (daemon_pids)) -eq 0
                for f in $socket $pidfile
                    if test -e $f
                        rm -f $f
                        echo "spectral-teardown: removed stale $f"
                    end
                end
            end
          '';

        in
        {
          packages.default = spectral;
          packages.spectral = spectral;

          devShells.default = pkgs.mkShell {
            buildInputs = [
              rustToolchain
              pkgs.pkg-config
              pkgs.clang
              spectral-teardown
            ]
            ++ spectral.passthru.runtimeLibs;

            LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
            LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath spectral.passthru.runtimeLibs;
            RUST_BACKTRACE = "1";
          };

          # The pre-commit gate: `nix flake check` builds the package (whose
          # default checkPhase already runs `cargo test`), clippy with warnings
          # denied, and rustfmt. Lints use the nixpkgs toolchain — the one that
          # builds the package — which can drift from the dev shell's pinned
          # rust-overlay toolchain.
          checks = {
            build = spectral;

            clippy = spectral.overrideAttrs (old: {
              pname = "spectral-clippy";
              nativeBuildInputs = old.nativeBuildInputs ++ [ pkgs.clippy ];
              buildPhase = ''
                runHook preBuild
                cargo clippy --all-targets --locked -- --deny warnings
                runHook postBuild
              '';
              doCheck = false;
              # Deliberately no install hooks: the package's postInstall wraps
              # binaries that a lint-only derivation never produces.
              installPhase = "touch $out";
            });

            fmt =
              pkgs.runCommand "spectral-fmt"
                {
                  nativeBuildInputs = [
                    pkgs.cargo
                    pkgs.rustfmt
                  ];
                }
                ''
                  cd ${self}
                  export CARGO_HOME="$TMPDIR/cargo"
                  cargo fmt --check
                  touch $out
                '';
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
