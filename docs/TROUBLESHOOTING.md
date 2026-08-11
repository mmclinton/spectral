# Troubleshooting

## My microphone isn't listed

Click "Refresh", or see the in-app "Not seeing your device?" guide.

## An application doesn't offer the virtual mic

Many applications enumerate input devices only at startup. Enable Spectral
first, then restart the application.

## My multi-channel audio interface behaves oddly

When the session manager splits an interface's inputs into separate sources,
each shows up as its own labelled entry ("EVO4 Mic 1", "EVO4 Line 1", …) — pick
the one your mic is plugged into.

When the interface appears as a single multi-channel device instead (e.g.
"Analog Surround 4.0"), Spectral captures its native channel count and
averages only the first two channels into the virtual mic — on such
interfaces those are the physical mic preamps, while the later channels
usually carry loopback or monitor returns that must not bleed into the
microphone. Devices with more than eight channels are captured as their
first eight.

## The device list is stale after a PipeWire restart

After something like `systemctl --user restart pipewire`, the device list points
at devices that no longer exist. Click "Refresh" and re-select before enabling.

## Something failed and I want to know why

Errors appear in the strip under the app bar. The daemon also logs to
`$XDG_RUNTIME_DIR/spectral-daemon.log`.

## Where are my voice-test recordings?

Under `~/.local/state/spectral/recordings`.

## Known limitations

- Disabling Spectral does not restore your previous default input. When the
  virtual mic disappears, WirePlumber picks a fallback, which may not be the
  microphone you started with — re-select it in your sound settings if it
  matters.
- Inputs with more than two channels are untested. Bluetooth microphones work —
  validated with AirPods Pro 2 — but no other Bluetooth device or codec has been
  tried.
- Voice-test recordings are not restored in the UI after relaunching the GUI.

## Reporting a bug

Issues go to [Codeberg](https://codeberg.org/mmc/spectral/issues). A useful
report includes:

- your distribution;
- your PipeWire and WirePlumber versions (`pipewire --version`,
  `wireplumber --version`);
- the input device involved;
- the daemon log from `$XDG_RUNTIME_DIR/spectral-daemon.log`.
