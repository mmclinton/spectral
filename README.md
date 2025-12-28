# Spectral

A real-time audio noise suppression application powered by [RNNoise](https://arxiv.org/pdf/1709.08243), a hybrid DSP and neural-network–based noise suppression system originally developed by Xiph.org.

**Status:** Functional on Linux via PipeWire. The application consists of a GUI (`spectral`) and a background daemon (`spectral-daemon`) that handles audio processing.

## Features

- Real-time noise suppression using RNNoise neural network
- PipeWire native audio pipeline integration
- Virtual microphone creation for use with any application
- Adjustable noise suppression threshold
- Persistent state across sessions

## Architecture

- **spectral** - GUI application for controlling noise suppression
- **spectral-daemon** - Background service that manages audio processing
- IPC communication via Unix domain sockets

## Requirements

- Linux with PipeWire
- PulseAudio compatibility layer (for module loading)
- Rust 2024 edition

## Development

Enter the development environment:
```bash
nix develop
```

Build the project:
```bash
nix build
```

Run the project:
```bash
nix run
```

Or use standard Cargo commands:
```bash
cargo build
cargo run
```

## Usage

1. Start the daemon: `spectral-daemon`
2. Launch the GUI: `spectral`
3. Select your input device from the dropdown
4. Adjust the noise suppression threshold as needed
5. Click "Enable" to create the virtual microphone
6. Select "Spectral-Mic" (or your custom name) as the input in other applications
