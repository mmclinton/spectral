# Command-line interface

The `spectral` binary doubles as a command-line client, so nothing about
Spectral requires the GUI. Running it without a command opens the control
panel; running it with one talks to the daemon and exits. `enable` starts
the daemon if none is running, as the GUI does; `disable`, `shutdown` and
`status` report that none is running instead, and `devices` reads the
PipeWire registry directly and needs no daemon at all.

```bash
spectral devices              # list input devices: id, channels, name
spectral enable --device 62   # create the virtual mic
spectral enable               # re-enable with the remembered settings or defaults
spectral status               # show state, device, mic name, threshold, toggles
spectral disable              # tear the virtual mic down
spectral shutdown             # stop the daemon
```

`spectral --help` prints the same reference; `spectral --version` prints the
version.

## `enable` flags

| Flag | Meaning |
| --- | --- |
| `--device <id>` | Input device id, as listed by `spectral devices`. Defaults to the remembered device. |
| `--threshold <0-100>` | Voice gate threshold; `0` means denoise only, with the gate off. Higher values gate more strictly on voice activity. |
| `--mic-name <name>` | Virtual mic name. `[A-Za-z0-9_-]`, at most 20 characters. |
| `--rumble-filter <on\|off>` | Cut low-frequency rumble (below ~90 Hz) before denoising. |
| `--auto-level <on\|off>` | Even out voice volume automatically, with a peak limiter as the safety ceiling. |

Every flag is optional, and an omitted flag keeps whatever the daemon already
has — the prior setting, or the default on a first run. That is what makes a
bare `spectral enable` a "restore what I had" command.

## Device ids

Device ids come from PipeWire and are scoped to the running PipeWire instance:
they change when PipeWire restarts, and a stale id will not resolve. The daemon
sidesteps this by also remembering the selected device by its node name and
re-resolving from that, so a flagless `spectral enable` keeps working across
reboots and PipeWire restarts. Re-run `spectral devices` before passing an
explicit `--device`.

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | Success, including a `disable` or `shutdown` that had nothing to do. |
| `1` | Runtime failure — including `status` when no daemon is running. |
| `2` | Usage error. |

## Under Flatpak

A Flatpak install exports the CLI as `page.codeberg.mmc.Spectral` rather than
`spectral`; see [INSTALLATION.md](INSTALLATION.md#flatpak).
