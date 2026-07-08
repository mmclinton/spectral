# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.0] - 2026-07-08

### Added
- The window now shows an error line under the status bar when an action
  fails, including when the daemon cannot be reached. Previously errors went
  to stderr only, which a desktop launch never shows.
- Launching Spectral without a reachable daemon now opens the window with an
  explanation instead of exiting before any window appeared.

### Changed
- The virtual microphone is now a native PipeWire source owned by the
  daemon instead of PulseAudio null-sink + remap-source modules. Exactly
  one new device appears — an input; the "Spectral-DoNotUse" output device
  is gone — and the mic vanishes the instant the daemon exits, so leaked
  devices are impossible by construction. The default input is selected
  through PipeWire metadata (the same value `wpctl set-default` writes),
  and the pipeline now requests a 10 ms quantum, cutting end-to-end
  latency when the graph grants it.
- The input threshold now spans the whole slider: 0% is the explicit
  denoise-only baseline (voice gate off) and 1–100% map linearly to a VAD
  confidence cutoff of up to 0.95. Previously 0–49% were all identical to
  denoise-only and only 50–100% drove the gate. Values 50–100% keep almost
  exactly their old meaning (the default of 80% moves from a 0.77 to a 0.76
  cutoff); values 1–49% go from doing nothing to a lenient gate.

### Removed
- The `--reap` subcommand and all PulseAudio module handling, including
  the libpulse dependency — with the mic's objects owned by the daemon
  process there is nothing left to leak or reap. One-time note when
  upgrading from 0.2.x: modules leaked by old versions are no longer swept
  automatically. Any 0.2.3 daemon start already cleaned them; a lingering
  "Spectral-DoNotUse"/"Spectral-Mic" pair can be removed with
  `pactl unload-module module-remap-source` then
  `pactl unload-module module-null-sink`, or a session restart.

### Fixed
- The daemon now detects when the audio pipeline dies (for example after a
  PipeWire restart), tears down the dead virtual mic instead of leaving a
  silent zombie device selectable, and reports the reason. Previously the
  status stayed Active while the mic produced silence.
- The GUI now reconciles its status display with the daemon every second
  instead of trusting its last click, so a pipeline or daemon death shows up
  within about a second. Voice-test playback still polls at 250 ms.
- Disabling now reports virtual-mic teardown failures (the leftover modules
  are reaped on the next enable) instead of always claiming success.
- Out-of-range threshold values from a hand-edited settings file or a rogue
  socket client are now clamped to 0–100; previously they produced a cutoff
  above 1.0 that the voice detector can never reach — a permanently silent
  virtual mic.
- The Help view's threshold entry described the slider as adjusting the
  noise-suppression strength; it now explains the actual voice-gate
  mechanism (per-frame VAD confidence against a cutoff).
- Starting a voice test no longer leaks the previous recording's `pw-record`
  processes. Previously a start without an intervening stop (for example after
  relaunching the GUI mid-recording) orphaned the old pair, which kept running
  and kept writing to the same output files as the new pair; partial spawn
  failures could likewise orphan the recorder that did start.

### Internal
- The real-time audio path no longer takes locks or allocates: the threshold
  is an atomic, the denoiser state is owned by the pipeline thread, and both
  stream callbacks reuse preallocated buffers.
- Node names are resolved from the PipeWire registry instead of shelling out
  to `wpctl`, dropping the WirePlumber CLI from the daemon's runtime
  dependencies.
- Enable's readiness gate now verifies both streams and the mic's feed link
  natively; the previous gate only waited for the PulseAudio source to
  register.
- The daemon's state query now takes its locks one at a time instead of holding
  all three simultaneously in reverse of the documented lock order, closing a
  latent deadlock trap.

## [0.2.3] - 2026-07-06

### Changed
- Locating the daemon socket now requires `XDG_RUNTIME_DIR` and fails with a clear
  error when it is unset. The previous `/tmp/runtime-$USER` fallback pointed at a
  directory that was never created, so connecting there could never succeed.

### Fixed
- Startup and enable no longer rely on fixed sleeps: the GUI retries the daemon
  connection with a bounded deadline after spawning it, waits for an old daemon to
  actually exit before respawning one, and the daemon polls PulseAudio until the
  virtual source is registered before selecting it as the default input.
- Voice-test recordings now live under `~/.local/state/spectral/recordings` (the XDG
  state directory) instead of `~/.config/spectral/recordings`, keeping WAV audio out
  of config-directory backups. The two old files are swept automatically on daemon
  startup.

### Internal
- The UI derives control enablement from an explicit `mic_enabled` property instead
  of comparing the status color against red.
- The dev shell consumes the package's runtime library list via `passthru` instead
  of maintaining a duplicate copy in the flake.
- `nix flake check` now actually gates: it builds the package (which runs the test
  suite), lints with clippy under denied warnings, and verifies formatting.
- Removed the PulseAudio connect success print (error-only logging convention), a
  stale cross-reference comment, and the Windows `USERPROFILE` fallback from the
  Linux-only persistence module.

## [0.2.2] - 2026-06-30

### Fixed
- The running application window now shows the Spectral icon in the Wayland taskbar/dock
  instead of GNOME's generic fallback glyph. The XDG app id is now set after the window's
  platform context is initialized — previously the call ran before any Slint context
  existed, silently failed, and never reached the compositor, so GNOME could not match the
  live window to its desktop file. The app id, desktop file, `Icon=`, and `StartupWMClass`
  now use the reverse-DNS name `page.codeberg.mmc.Spectral`. (#1)

## [0.2.1] - 2026-06-30

### Changed
- Updated the project icon.

### Internal
- Removed dead code left over from earlier feature removals: the inert signal-handler mpsc
  channel, the uncalled `sanitize_mic_name` helper, a no-op `SPECTRAL_RENDERER_ATTEMPT`
  environment cleanup, and the unread `is_recording`/`has_recording` fields from the daemon
  IPC state.
- Removed the placeholder `macos`/`windows` platform module declarations (Spectral is
  Linux-only), which restores the `cargo fmt --check` gate.

## [0.2.0] - 2026-06-29

### Added
- `spectral-daemon --reap` subcommand, plus automatic cleanup of leaked virtual-mic
  modules at daemon startup, before each enable, and from `spectral-teardown`.
- Mic-name input validation with live keystroke rejection in the GUI.

### Changed
- Enabling now fails with a clear error when the audio pipeline cannot start, instead of
  presenting an active-but-silent virtual microphone.
- Settings are persisted when you click Enable; the unused active-state restore was removed
  and the daemon always starts inactive.
- The daemon recovers from a poisoned lock instead of crashing, for consistent behavior
  across its threads.
- Pinned Slint to 1.15.0 using the software renderer only.

### Fixed
- Duplicate and stale "Spectral-DoNotUse" outputs / "Spectral-Mic" inputs no longer
  accumulate in the system audio settings — leaked PulseAudio modules are reaped.
- Starting a recording with no input device selected now returns a clear error instead of
  silently dropping the connection.

### Documentation
- README: removed the Development section and corrected the Usage steps (the GUI starts the
  daemon automatically).

### Internal
- Removed dead IPC commands and pending-state machinery; deduplicated the wpctl node lookup,
  virtual-mic node naming, the GUI Active/Pending recompute, and the PulseAudio
  wait-with-timeout pattern; added the `spectral-teardown` dev helper.

## [0.1.0] - 2026-01-01

### Added
- Real-time noise suppression using RNNoise neural network
- PipeWire native audio pipeline integration
- Virtual microphone creation for use with any application
- Adjustable noise suppression threshold with VAD gating
- Persistent state across sessions
- GUI application (Slint-based) for controlling noise suppression
- Background daemon for audio processing
- IPC communication via Unix domain sockets
- NixOS module and overlay for declarative installation
- Recording and playback comparison feature

### Notes
- Nix is the only officially supported installation method
- Requires PipeWire (with the PulseAudio compatibility layer) and WirePlumber

[Unreleased]: https://codeberg.org/mmc/spectral/compare/v0.3.0...HEAD
[0.3.0]: https://codeberg.org/mmc/spectral/compare/v0.2.3...v0.3.0
[0.2.3]: https://codeberg.org/mmc/spectral/compare/v0.2.2...v0.2.3
[0.2.2]: https://codeberg.org/mmc/spectral/compare/v0.2.1...v0.2.2
[0.2.1]: https://codeberg.org/mmc/spectral/compare/v0.2.0...v0.2.1
[0.2.0]: https://codeberg.org/mmc/spectral/compare/v0.1.0...v0.2.0
[0.1.0]: https://codeberg.org/mmc/spectral/releases/tag/v0.1.0