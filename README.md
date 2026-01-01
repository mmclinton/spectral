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

## Installation

> **Nix is the only officially supported installation method.**  
> Spectral depends on PipeWire, PulseAudio (libpulse), and WirePlumber being correctly configured. The Nix package handles this automatically. Manual builds may work but are unsupported. I am currently working on broadening support.

### NixOS / Nix

#### Option 1: Direct installation (try it out)

```bash
# Run directly without installing
nix run git+https://codeberg.org/mmc/spectral

# Or build and install to your profile
nix profile add git+https://codeberg.org/mmc/spectral
```

#### Option 2: NixOS declarative installation

Add to your `flake.nix` inputs:

```nix
{
  inputs = {
    spectral.url = "git+https://codeberg.org/mmc/spectral";
  };
}
```

Then in your NixOS configuration:

```nix
{ inputs, ... }:
{
  imports = [ inputs.spectral.nixosModules.default ];

  # Apply the overlay to make pkgs.spectral available
  nixpkgs.overlays = [ inputs.spectral.overlays.default ];

  # Enable Spectral
  programs.spectral.enable = true;
}
```

#### Option 3: Using the overlay only

If you prefer manual control over installation:

```nix
{
  nixpkgs.overlays = [ inputs.spectral.overlays.default ];

  # Then use pkgs.spectral anywhere
  environment.systemPackages = [ pkgs.spectral ];
}
```

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
