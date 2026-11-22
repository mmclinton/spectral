# Command-line interface

The `spectral` binary is also a command-line client for everything but the voice test, which needs the GUI. Without a command it opens the control panel; with one it talks to the daemon and exits.

| Command | What it does | With no daemon running |
| --- | --- | --- |
| `spectral devices` | Lists input devices: id, channels, name | Needs none; it reads the PipeWire registry |
| `spectral enable` | Creates the virtual mic ([flags](#enable-flags) below) | Starts one, as the GUI does |
| `spectral status` | Shows the state, device, mic name, threshold and toggles | Reports that none is running |
| `spectral disable` | Tears the virtual mic down | Reports that none is running |
| `spectral shutdown` | Stops the daemon | Reports that none is running |

`spectral --help` prints the same reference, and `spectral --version` the version. `man spectral` covers the same ground and adds the file locations. A Flatpak has neither the man page nor the shell completions, and its command takes another name: see [packaging/flatpak](../packaging/flatpak/README.md).

## Shell completions

Fish, zsh and bash complete the subcommands, every flag, the `on`/`off` values, and the live device ids for `--device`; fish and zsh show each device's name beside its id. The Nix package installs all three. From a checkout, `source assets/spectral.fish` or `source assets/spectral.bash` loads them for the session, as does `source assets/spectral.zsh` once `compinit` has run.

## `enable` flags

| Flag | Meaning |
| --- | --- |
| `--device <id>` | Input device id, as listed by `spectral devices`. Required the first time; afterwards defaults to the remembered device. |
| `--threshold <0-100>` | Voice gate threshold; `0` means denoise only, with the gate off. Higher values gate more strictly on voice activity. |
| `--mic-name <name>` | Virtual mic name. `[A-Za-z0-9_-]`, at most 20 characters. |
| `--rumble-filter <on\|off>` | Cut low-frequency rumble (below ~90 Hz) before denoising. |
| `--auto-level <on\|off>` | Even out voice volume automatically, with a peak limiter as the safety ceiling. |

Every flag but a first run's `--device` is optional, and an omitted flag keeps whatever the daemon already has — the prior setting, or the default on a first run. That is what makes a bare `spectral enable` a "restore what I had" command:

```bash
spectral enable --device 62   # the first run picks the input
spectral enable               # later runs restore the remembered settings
```

## Device ids

Device ids come from PipeWire and are scoped to the running PipeWire instance: they change when PipeWire restarts, and a stale id will not resolve. The daemon sidesteps this by also remembering the selected device by its node name and re-resolving from that, so a flagless `spectral enable` keeps working across reboots and PipeWire restarts. Re-run `spectral devices` before passing an explicit `--device`.

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | Success, including a `disable` or `shutdown` that had nothing to do. |
| `1` | Runtime failure — including `status` when no daemon is running. |
| `2` | Usage error. |
