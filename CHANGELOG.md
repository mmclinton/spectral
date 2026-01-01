# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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