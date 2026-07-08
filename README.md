# Spectral

A real-time audio noise suppression application powered by [RNNoise](https://arxiv.org/pdf/1709.08243), a hybrid DSP and neural-network–based noise suppression system originally developed by Xiph.org.

**Status:** Functional on Linux via PipeWire. The application consists of a GUI (`spectral`) and a background daemon (`spectral-daemon`) that handles audio processing.

## Features

- Real-time RNNoise denoising with an adjustable voice-activity gate
- Native PipeWire virtual microphone, owned by the daemon — it disappears the instant the daemon exits, so stale devices cannot accumulate
- Automatically set as the system default input on Enable
- Built-in voice test to compare original and filtered audio
- Persistent settings across sessions

## Architecture

- **spectral** - GUI application for controlling noise suppression
- **spectral-daemon** - Background service that manages audio processing
- IPC communication via Unix domain sockets

## Installation

> Nix and the AUR are the supported installation methods.
> Spectral requires a running PipeWire session with its session manager
> (WirePlumber). Manual builds may work but are unsupported.

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

### Arch Linux (AUR)

```bash
paru -S spectral-mic   # or: yay -S spectral-mic
```

The package is named `spectral-mic` and conflicts with the unrelated AUR
package `spectral`, which ships the same binary name. Publication to the AUR
begins with the first announced release; until then the PKGBUILD lives in
[packaging/aur](packaging/aur).

## Usage

1. Launch the GUI: `spectral` (the daemon starts automatically)
2. Select your input device from the list
3. Adjust the threshold: 0% is denoise-only, higher values gate more strictly on voice activity
4. Click "Enable" — the virtual microphone is created and becomes the system default input
5. Applications that pin a specific device can select the virtual mic by name (default "Spectral-Mic"); "Rename"/"Reset" stage a new name, applied on the next Enable/Apply

## Troubleshooting

- **Your microphone isn't listed:** click "Refresh", or see the in-app "Not seeing your device?" guide.
- **After a PipeWire restart** (e.g. `systemctl --user restart pipewire`): the device list points at devices that no longer exist — click "Refresh" and re-select before enabling.
- **Something failed:** errors appear under the status bar; the daemon also logs to `$XDG_RUNTIME_DIR/spectral-daemon.log`.
- **Voice-test recordings** are saved under `~/.local/state/spectral/recordings`.

## Known limitations

- The window's minimum size is 920×800, which does not fit 768p displays. A responsive-layout fix is planned.
- Inputs with more than two channels and Bluetooth microphones are untested.
- Voice-test recordings are not restored in the UI after relaunching the GUI.

## Non-goals

- Denoising incoming audio (a "headphones sink") — [EasyEffects](https://github.com/wwmm/easyeffects) covers this well.
- Systems without PipeWire (pure PulseAudio, ALSA, or JACK).
- Self-updating — package managers own updates.

## Rendering

Spectral forces Slint to the `winit` backend with the `software` renderer,
and this is the only renderer compiled into the binary. This combination
has the broadest coverage: it has no GPU, EGL, Vulkan, GBM, or libdrm
dependency and therefore survives broken or mismatched Mesa/driver
stacks, headless sessions, and most container/sandbox setups. Spectral's
UI is small and static, so CPU rasterization is not noticeable in
practice.

`SLINT_BACKEND` is set unconditionally at startup, so the environment
variable cannot be used to switch renderers. If you want a different
combination, you must rebuild Spectral with the matching
`slint/renderer-...` feature enabled and remove the override in
`src/main.rs`.

## License

Spectral is licensed under the GPL-3.0-only (see [LICENSE](LICENSE)).
Statically linked third-party components are attributed in
[THIRD-PARTY.md](THIRD-PARTY.md).
