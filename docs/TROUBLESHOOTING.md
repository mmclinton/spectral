# Troubleshooting

## My microphone isn't listed

Click "Refresh", or see the in-app "Not seeing your device?" guide.

## An application doesn't offer the virtual mic

Many applications enumerate input devices only at startup. Enable Spectral first, then restart the application.

## My multi-channel audio interface behaves oddly

When the session manager splits an interface's inputs into separate sources, each shows up as its own labeled entry ("EVO4 Mic 1", "EVO4 Line 1", …) — pick the one your mic is plugged into.

When the interface appears as a single multi-channel device instead (e.g. "Analog Surround 4.0"), Spectral captures its native channel count and averages only the first two channels into the virtual mic — on such interfaces those are the physical mic preamps, while the later channels usually carry loopback or monitor returns that must not bleed into the microphone. Plug your mic into one of the first two inputs.

## My microphone was unplugged

Spectral keeps the virtual mic in place and goes quiet for about 30 seconds, so whatever is using it does not lose the device. Plug the microphone back in within that window and it picks up where it left off. Past it the mic stops, and you enable it again once the device is back.

## The device list is stale after a PipeWire restart

After something like `systemctl --user restart pipewire`, the list shows devices that no longer exist. Enabling still works — Spectral remembers your device by a name that survives the restart — but click "Refresh" if you want the list itself to be accurate before picking a different device.

## Something failed and I want to know why

An "Error" chip appears in the app bar next to the status. Click it for the full message and a diagnostics summary. The daemon also logs to `$XDG_RUNTIME_DIR/spectral-daemon.log`.

On a Flatpak install that path is inside the sandbox, so a host terminal cannot open it. Read it through the sandbox instead:

```bash
flatpak run --command=cat page.codeberg.mmc.Spectral \
    /run/user/$(id -u)/spectral-daemon.log
```

## Where are my voice-test recordings?

Under `~/.local/state/spectral/recordings`.

## Known limitations

- Disabling Spectral does not restore your previous default input. When the virtual mic disappears, WirePlumber picks a fallback, which may not be the microphone you started with — re-select it in your sound settings if it matters.
- Channel handling is validated for mono and stereo inputs only; the multi-channel path described above is untested. Bluetooth microphones work — validated with AirPods Pro 2 — but no other Bluetooth device or codec has been tried.
- Voice-test recordings are not restored in the UI after relaunching the GUI.

## Reporting a bug

Issues go to [Codeberg](https://codeberg.org/mmc/spectral/issues). A useful report includes:

- your distribution;
- your PipeWire and WirePlumber versions (`pipewire --version`, `wireplumber --version`);
- the input device involved;
- the Diagnostics box from the app bar's error details, if an error is showing;
- the daemon log, from `$XDG_RUNTIME_DIR/spectral-daemon.log` or, under Flatpak, through the command [above](#something-failed-and-i-want-to-know-why).
