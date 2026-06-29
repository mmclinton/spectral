# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

[Unreleased]: https://codeberg.org/mmc/spectral/compare/v0.2.0...HEAD
[0.2.0]: https://codeberg.org/mmc/spectral/compare/v0.1.0...v0.2.0
[0.1.0]: https://codeberg.org/mmc/spectral/releases/tag/v0.1.0