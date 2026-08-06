<div align="center">
  <img src="./assets/spectral.png" alt="Spectral icon" width="200">
  <p>
    <a href="https://codeberg.org/mmc/spectral/actions?workflow=ci.yml">
      <img src="https://codeberg.org/mmc/spectral/badges/workflows/ci.yml/badge.svg?branch=main" alt="CI status">
    </a>
    <a href="LICENSE">
      <img src="https://img.shields.io/badge/license-GPL--3.0--only-blue" alt="License: GPL-3.0-only">
    </a>
  </p>
</div>

# Spectral

**Real-time microphone noise suppression for Linux.**

Spectral runs a microphone you choose through the RNNoise neural network and
exposes the result as a native PipeWire virtual microphone. Any application —
your browser, a chat client, OBS — can select it like any other input. Keyboard
clatter, fans, and room tone go; your voice stays.

[RNNoise](https://arxiv.org/pdf/1709.08243) is a hybrid DSP and neural-network
noise suppressor by Jean-Marc Valin, developed at Mozilla and published through
Xiph.Org.

> **Status:** pre-1.0 and actively developed. The CLI surface and the settings
> file may still change between releases; see [CHANGELOG.md](CHANGELOG.md).

<div align="center">
  <img src="./assets/screenshot.png" width="820"
       alt="Spectral's main window: an app bar with the status, a full-width input threshold band, then two columns of cards holding the input device list, the virtual mic name, the voice-processing switches and the voice test, above a footer with Disable and Apply">
</div>

<details>
<summary>Table of Contents</summary>

- [Features](#features)
- [Requirements](#requirements)
- [Installation](#installation)
- [Usage](#usage)
- [How it works](#how-it-works)
- [Troubleshooting](#troubleshooting)
- [Known limitations](#known-limitations)
- [How Spectral compares](#how-spectral-compares)
- [Non-goals](#non-goals)
- [Contributing](#contributing)
- [Privacy](#privacy)
- [License](#license)

</details>

## Features

- Real-time RNNoise denoising, with a voice-activity gate you can dial from
  off (denoise only) to strict
- A native PipeWire virtual microphone that any application can select
- A built-in voice test — record yourself and compare original against filtered
- Becomes the system default input automatically when you enable it
- Two optional voice-processing switches, both off by default: a rumble filter
  and automatic volume leveling
- Settings persist across restarts, and your device is remembered by name, so
  it survives reboots and PipeWire restarts
- A headless CLI, so usage does not require the GUI

## Requirements

- Linux with **PipeWire** and its session manager, **WirePlumber**
- Wayland or X11
- x86_64 or aarch64

Confirmed working on x86_64 NixOS and on aarch64 Asahi Linux (Ubuntu on an
M1 MacBook Pro). Other PipeWire distributions on these architectures should
work but have not been validated.

## Installation

> Nix and the AUR are the supported installation methods. A Flatpak manifest
> is included and builds cleanly, but it has not yet been through a Flathub
> review. Manual builds may work but are unsupported.

<details>
<summary><strong>NixOS / Nix</strong></summary>

A nixpkgs package has been submitted and is in review. Until it lands, install
from this flake.

**Option 1: Direct installation (try it out)**

```bash
# Run directly without installing
nix run git+https://codeberg.org/mmc/spectral

# Or build and install to your profile
nix profile add git+https://codeberg.org/mmc/spectral
```

**Option 2: NixOS declarative installation**

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

**Option 3: Using the overlay only**

If you prefer manual control over installation:

```nix
{
  nixpkgs.overlays = [ inputs.spectral.overlays.default ];

  # Then use pkgs.spectral anywhere
  environment.systemPackages = [ pkgs.spectral ];
}
```

</details>

<details>
<summary><strong>Arch Linux (AUR)</strong></summary>

Build from the PKGBUILD in this repository:

```bash
git clone https://codeberg.org/mmc/spectral.git
cd spectral/packaging/aur
makepkg -si
```

The PKGBUILD fetches and builds the pinned latest release tarball
(currently v0.8.0), not the checkout it sits in, and declares x86_64
only — no aarch64 Arch system has been tested.

**Spectral is not on the AUR yet.** Submission of `spectral-mic` is
pending; once it lands, this becomes the install command:

```bash
paru -S spectral-mic   # or: yay -S spectral-mic — not available yet
```

The package is named `spectral-mic` rather than `spectral` because the
unrelated AUR package of that name ships a binary called `spectral` too.
The PKGBUILD is maintained here under [packaging/aur](packaging/aur).

</details>

<details>
<summary><strong>Flatpak</strong></summary>

**Spectral is not on Flathub yet.** Until it is, build the Flatpak
yourself — it needs only `flatpak` and the Flathub remote:

```bash
flatpak install -y flathub org.flatpak.Builder
git clone https://codeberg.org/mmc/spectral.git
cd spectral
flatpak run --command=flathub-build org.flatpak.Builder --install \
    packaging/flatpak/page.codeberg.mmc.Spectral.yaml
flatpak run page.codeberg.mmc.Spectral
```

One difference from the native packages is worth knowing before you
choose this route: Flatpak exports a single command per application,
named after the app ID, so the headless CLI answers to
`page.codeberg.mmc.Spectral` rather than to `spectral`:

```bash
page.codeberg.mmc.Spectral devices
```

Alias it if the full name grates — Flatpak has no way to export a shorter
one.

Details, including the full permission set and what a Flathub submission
would change, are in [packaging/flatpak](packaging/flatpak).

</details>

## Usage

1. Launch the GUI: `spectral` (the daemon starts automatically)
2. Select your input device from the list
3. Adjust the threshold: higher values gate more strictly on voice activity
4. Optionally flip any of the Voice Processing switches
5. Click "Enable" — the virtual microphone is created and becomes the system default input
6. Applications that pin a specific device can select the virtual mic by name (default "Spectral-Mic"); "Rename"/"Reset" stage a new name, applied on the next Enable/Apply

### Headless CLI

The `spectral` binary doubles as a command-line client:

```bash
spectral devices              # list input devices: id, channels, name
spectral enable --device 62   # create the virtual mic
spectral enable               # re-enable with the remembered settings or defaults
spectral status               # show state, device, mic name, threshold, toggles
spectral disable              # tear the virtual mic down
spectral shutdown             # stop the daemon
```

`enable` accepts:

- `--device <id>`
- `--threshold <0-100>`
- `--mic-name <name>`
- `--rumble-filter <on|off>`
- `--auto-level <on|off>`

Omitted flags keep the prior settings or defaults. Device ids are scoped to the
running PipeWire instance — the daemon remembers the selected device by node
name, so a flagless `spectral enable` works across reboots and PipeWire
restarts.

Exit codes: 0 on success (including a no-op `disable`/`shutdown`), 1 on runtime
failures (including `status` when no daemon is running), 2 on usage errors.

## How it works

A background daemon owns all the audio work. It captures your chosen device,
downmixes to mono, and runs each 10 ms frame through the processing chain:
the optional rumble filter, then RNNoise, then the optional leveler, then the
voice gate. The result feeds a PipeWire virtual source node — the microphone
other applications see.

The GUI and the CLI are thin clients. They talk to the daemon over a Unix
socket and hold no audio state of their own. Because the daemon owns every
PipeWire object it creates, the virtual mic disappears cleanly when the daemon
stops; there is nothing left behind in the audio graph to clean up.

For the full picture — module map, the real-time constraints, the IPC
protocol — see [ARCHITECTURE.md](ARCHITECTURE.md).

## Troubleshooting

- **Your microphone isn't listed:** click "Refresh", or see the in-app "Not seeing your device?" guide.
- **An application doesn't offer the virtual mic:** many applications enumerate input devices only at startup. Enable Spectral first, then restart the application.
- **Multi-channel audio interfaces:** when the session manager splits an interface's inputs into separate sources, each shows up as its own labelled entry ("EVO4 Mic 1", "EVO4 Line 1", …) — pick the one your mic is plugged into. When the interface appears as a single multi-channel device instead (e.g. "Analog Surround 4.0"), only the first two capture channels are used; for a single mic, you'll get the best level selecting a mono or stereo profile for the interface in your sound settings.
- **After a PipeWire restart** (e.g. `systemctl --user restart pipewire`): the device list points at devices that no longer exist — click "Refresh" and re-select before enabling.
- **Something failed:** errors appear in the strip under the app bar; the daemon also logs to `$XDG_RUNTIME_DIR/spectral-daemon.log`.
- **Voice-test recordings** are saved under `~/.local/state/spectral/recordings`.

## Known limitations

- Disabling Spectral does not restore your previous default input. When the virtual mic disappears, WirePlumber picks a fallback, which may not be the microphone you started with — re-select it in your sound settings if it matters.
- Inputs with more than two channels are untested. Bluetooth microphones work — validated with AirPods Pro 2 — but no other Bluetooth device or codec has been tried.
- Voice-test recordings are not restored in the UI after relaunching the GUI.

## How Spectral compares

If you're coming from **NoiseTorch**: it solved this problem for PulseAudio and
is no longer maintained. Spectral targets PipeWire natively rather than loading
PulseAudio modules, and adds a daemon with both a GUI and a CLI. Same core idea,
same RNNoise denoiser.

**EasyEffects** is a broader audio-effects suite that can do far more than
noise suppression, including on output. Spectral does one thing. If you already
run EasyEffects and are happy with it, you don't need this.

## Non-goals

- Denoising incoming audio (a "headphones sink") — [EasyEffects](https://github.com/wwmm/easyeffects) covers this well.
- Systems without PipeWire (pure PulseAudio, ALSA, or JACK).
- Self-updating — package managers own updates.

## Contributing

Bug reports and patches are welcome. Development happens on
[Codeberg](https://codeberg.org/mmc/spectral); see
[CONTRIBUTING.md](CONTRIBUTING.md) for the build, the required checks, and the
commit conventions, and [ARCHITECTURE.md](ARCHITECTURE.md) for a map of the
codebase.

A useful bug report includes your distribution, your PipeWire and WirePlumber
versions (`pipewire --version`, `wireplumber --version`), the input device
involved, and the daemon log from `$XDG_RUNTIME_DIR/spectral-daemon.log`.

## Privacy

All processing happens on your machine. Spectral makes no network connections
and has no telemetry. The only audio it writes to disk comes from voice-test
recordings you start yourself, under `~/.local/state/spectral/recordings`.

## License

Spectral is licensed under the GPL-3.0-only (see [LICENSE](LICENSE)).
Statically linked third-party components are attributed in
[THIRD-PARTY.md](THIRD-PARTY.md).
