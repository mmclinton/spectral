# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed
- Locating the daemon socket now requires `XDG_RUNTIME_DIR` and fails with a clear
  error when it is unset. The previous `/tmp/runtime-$USER` fallback pointed at a
  directory that was never created, so connecting there could never succeed.

### Fixed
- Startup and enable no longer rely on fixed sleeps: the GUI retries the daemon
  connection with a bounded deadline after spawning it, waits for an old daemon to
  actually exit before respawning one, and the daemon polls PulseAudio until the
  virtual source is registered before selecting it as the default input.

### Internal
- The UI derives control enablement from an explicit `mic_enabled` property instead
  of comparing the status color against red.

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

[Unreleased]: https://codeberg.org/mmc/spectral/compare/v0.2.2...HEAD
[0.2.2]: https://codeberg.org/mmc/spectral/compare/v0.2.1...v0.2.2
[0.2.1]: https://codeberg.org/mmc/spectral/compare/v0.2.0...v0.2.1
[0.2.0]: https://codeberg.org/mmc/spectral/compare/v0.1.0...v0.2.0
[0.1.0]: https://codeberg.org/mmc/spectral/releases/tag/v0.1.0