# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- A manual page, so `man spectral` now works. It documents the commands, the `enable` flags, the exit codes, and where Spectral keeps your settings, its control socket, its log and your voice-test recordings.
- Completions for fish, zsh and bash, covering the commands and every flag. `--device` completes to the input devices currently on your system, so there is no need to run `spectral devices` first and copy an id; fish and zsh list each one with its name, while bash, which has no way to show one, offers the ids alone. `--rumble-filter` and `--auto-level` complete to `on` and `off`. The Nix package installs both the page and the completions for you; the Flatpak does not, since Flatpak exports neither to the host and its command is named `page.codeberg.mmc.Spectral`.
- A mistyped command or flag now suggests what you probably meant: `spectral enalbe` answers with "did you mean 'enable'?" instead of only saying the word was not recognized. Nothing is suggested when no command is close enough for the guess to be worth making.

### Changed
- The interface now ships its own font, Iosevka Aile, instead of using whichever monospaced family your system prefers. Every install renders the same way, and the face is a quasi-proportional one designed for reading rather than a terminal font. The embedded faces are cut down to the characters the interface draws; a device name in another script still renders, in whatever system font supplies those glyphs.
- The daemon's pipeline thread no longer wakes a hundred times a second just to keep the microphone on. It used to poll for a stop request every 10 ms; it now sleeps until something happens, which lets a laptop's CPU rest properly between audio callbacks. Nothing about the audio changes.
- A new application icon: a silver microphone in a blue cradle on a dark disc. It replaces the orange and teal outline drawing, so Spectral looks different in your launcher and dock after updating.
- Bumped Slint from 1.17.1 to 1.18.1. Its software renderer, the one Spectral draws with, spaces glyphs evenly instead of leaving uneven gaps between letters, and on Wayland the window's first frame now comes up at the right size, and redraws no longer leave artifacts when several parts of the window change at once. The MSRV stays at 1.92.

### Fixed
- The audio thread no longer takes page faults during the first second after enabling the mic. Its staging buffers were reserved but not yet touched, so the kernel mapped them in on the first audio callbacks; they are now written in full before the pipeline starts.
- An input device that reports an empty name to PipeWire is now listed as "Unknown" rather than with a blank name. It appeared in the device list and in `spectral devices` as a nameless row, which was easy to mistake for a broken listing.
- Pressing Ctrl-C in, or closing, the terminal you started the window from no longer takes the virtual mic down with it. A daemon the window started shared the terminal's signals, so whether the mic outlived the window depended on how it had been launched.
- An error that has been resolved elsewhere, for example by a `spectral enable` or `spectral disable` from a terminal, now leaves the window too. It used to stay on screen next to an Active status, along with its Error chip.
- When the window opens without being able to start the daemon, the reason now stays on screen. After a second it was replaced with a generic "restart Spectral" message, which cannot help when the cause is permanent, such as a missing `spectral-daemon` binary.
- A grayed-out button no longer acts on Space or Enter. A button that still had keyboard focus when it was disabled, such as Record after the microphone stopped, went on responding to the keyboard.
- `spectral shutdown` interrupted before it finished no longer leaves the daemon half shut down. The microphone was gone, but the daemon kept running and went on reporting it as Active.
- When the daemon drops or stalls a request, the command now says so, instead of printing "EOF while parsing a value" or "Resource temporarily unavailable (os error 11)".
- When the settings file exists but cannot be read, the daemon log now says so. Spectral used to start from defaults as though it were a first run, with nothing to explain why.
- The Help view now gives the Flatpak's location for voice-test recordings, alongside the usual one.
- Piping a command into something that stops reading early, such as `spectral status | head -1`, no longer ends in a panic and exit status 101. The command stops writing and exits with success.

## [0.9.0] - 2026-08-26

### Added
- The virtual microphone now survives its input device being unplugged. It stays available to whatever is using it and goes quiet for up to 30 seconds while it waits, then picks the device up again by itself when it is plugged back in — so a call carries on instead of losing the microphone. If the device does not come back within 30 seconds the microphone stops and says so.
- The status pill reads "Reconnecting" while the microphone is waiting for a disconnected input device, and `spectral status` says the same. The error area names the device that went away until it comes back.

### Removed
- The in-repo AUR PKGBUILD. Spectral's supported channels are nixpkgs and Flatpak; anyone is welcome to package the release tags for the AUR or any other format independently.
- Voice-test recordings left behind by 0.2.2 and older are no longer swept up. Those versions kept the recordings under the config directory; the daemon deleted the two stale files on every start so config backups stopped syncing audio. Anyone still upgrading from that far back can delete `~/.config/spectral/recordings/` by hand — current recordings live under the state directory and are unaffected.

### Security
- Voice-test recordings are no longer readable by other users on the same machine. The recordings directory and the two WAV files took whatever the umask allowed, so on a distribution whose home directories are world-readable (Debian and Ubuntu among them) any local account could read an unprocessed capture of your microphone. Existing files are narrowed the next time a recording is made.

### Fixed
- The voice test no longer keeps showing a recording in progress after the input device has been unplugged. The capture stops when the device goes, as it always did, but the countdown and the level meter ran on to the end as though nothing had happened.
- A voice test that failed no longer leaves its error on screen until you toggle the microphone off and on. Recording or playing back again clears it, the way enabling already did.
- Enabling the microphone again after unplugging and replugging it no longer fails with "Cannot use device 44: No Audio/Source node with id 44". PipeWire gives a device a new id every time it is plugged in, and the window was still sending the old one; it now sends the device's durable name instead. Refreshing the device list also keeps your selection across a replug rather than clearing it.
- The window, `spectral status` and the diagnostics dump no longer name or select the wrong microphone after a replug. All three identified the device by the id it was enabled with; PipeWire gives that id to whatever is plugged in next, so the picker could open on a webcam microphone while the real one was in use, and a dump could send a bug report after the wrong hardware. They now go by the device's durable name.
- The unsaved-changes chips no longer slide under the Disable button at the window's minimum width. Three chips did not fit the footer's left slot; the row now collapses to the count alone from the third change onward, as it already did from the fourth.
- A single corrupt sample from the capture device no longer silences the virtual mic until it is toggled off and on. Non-finite samples poisoned both the rumble filter and the denoiser permanently, and because the pipeline thread stayed alive the status kept reading Active while nothing came through.
- A daemon error no longer disappears for good if you refresh the device list, run a voice test, or press Apply while it is showing. The error chip now comes back on the next poll, and with it the route to the diagnostics summary, for as long as the daemon still reports the problem.
- Settings are no longer lost when two clients apply at the same time. The GUI and a `spectral enable` could interleave into one temp file and rename a spliced result over the settings, which then failed to parse and reset everything to defaults on the next start. Settings are also flushed to disk before being put into place, so a power cut cannot leave the file empty.
- Voice-test recordings can no longer be corrupted by a transient disk error. A failed checkpoint left the writer positioned in the WAV header, so the rest of the capture overwrote audio already recorded and the clip played back as garbage; recording now stops and reports the cause.
- The voice test now refuses a capture format it did not ask for instead of writing it at the wrong width, which produced clips that played back at the wrong speed with no error shown.
- `spectral enable --mic-name` (and the other flags) no longer swallow the next flag when their value is missing. `--mic-name --rumble-filter` used to create a virtual mic called "--rumble-filter", leave the filter off, and report success.
- A daemon shutting down no longer deletes the socket of the daemon replacing it, which left a running daemon nothing could connect to until it was killed by hand.
- A failure to reach a restarted daemon now reports why instead of a bare "Failed to get daemon state after restart".
- Unplugging the selected microphone no longer swaps it out from under you. The virtual mic could be relinked to whatever your system's default input had become — usually a built-in mic — and go on denoising and broadcasting from it, with the status still reading Active and the original device still named. A capture device that goes away while the mic is running is now reported as an error instead.
- Voice-test recordings now stop and report the problem when they cannot be written. A disk that filled mid-recording left the capture running with the error discarded, and could leave a truncated file presented as a complete one — including when the write only failed as the file was closed, which is where a full disk usually shows up.
- `XDG_RUNTIME_DIR` must now be set to an absolute path, and Spectral says so plainly when it is not. An empty value counted as set, which put the daemon's control socket, pid file and log in the current working directory instead of the runtime dir — outside the private directory that keeps other local users from reaching them. An unset value reported "No such file or directory" instead of naming the cause, and the daemon log no longer falls back to `~/tmp` or `/tmp`.
- Device listings reported 2 channels for every input; the count is now read from the device itself, so mono sources list as mono.
- The virtual mic always captured its input as stereo. It now negotiates the device's native channel count and verifies what PipeWire granted, so multi-channel interfaces keep their loopback/monitor returns out of the microphone.
- Closing the window now stops the voice test. A clip that was playing kept playing to its end after the window was gone, and a capture left running went on to the recorder's internal cap; both are now stopped on exit. The virtual mic is unaffected — it still outlives the window.
- The Help links now open on minimal systems: the Nix package puts `xdg-open` on the GUI's PATH instead of relying on the host to provide it.
- The close button on the Help and device-troubleshooting views, and the play buttons in the voice test, now center their marks. They were drawn as text characters, which a font places by its own metrics rather than by the button, and the close mark came from whichever fallback font happened to supply it.
- The diagnostics dump no longer sends Flatpak users after a log file their terminal cannot open. The path it names lives inside the sandbox, so the dump now says so and gives the command that reads it from the host.

### Changed
- The threshold band no longer dims while the mic is off. The threshold can be set before Enable, like the device and the mic name, so dimming it mislabeled a live control as inert.
- The rounded chips and the toggles carry a 2px border instead of 1px: the status pill, the error chip, the unsaved-changes chips, and the switch rail.
- The GUI feels more responsive: the daemon now waits for connections instead of polling for them every 100 ms, which removes up to that much delay from every status update the window makes.
- Reorganized `spectral --help`: the `enable` flags now have their own section instead of nesting under the command list, the header carries the version, and an examples block shows the three invocations most people need. Same commands, flags, and exit codes.
- Bumped Slint from 1.15.0 to 1.17.1 and fontique from 0.7 to 0.10. The MSRV rises to 1.92 as a result. Slint 1.17 reads the default font size from system settings on Linux, so typography may shift slightly under some desktop configurations.
- `spectral --help` and every invalid-mic-name rejection now quote one shared wording of the name rule instead of restating it per call site, so the text cannot drift from the check the daemon actually runs.
- Errors now surface as a clickable "Error" chip in the app bar instead of a one-line strip beneath it. The strip elided anything longer than the window with no way to read the rest; the chip opens a details view with the full message and a copyable diagnostics summary — versions, packaging, the log and config paths, and the daemon's last known state — for bug reports.

## [0.8.0] - 2026-07-31

### Added
- Clicking Refresh now spins the glyph briefly, acknowledging the rescan even when the device list comes back unchanged.

### Changed
- The control panel was redesigned around a resizable layout: an app bar (wordmark, status pill, links), a full-width threshold band with the gate's meaning and scale anchors inline, two columns of cards that stretch with the window, and a footer holding the actions. The old fixed-size canvas and its floating controls are gone.
- Unsaved changes are now visible: the footer names each staged setting that differs from what the daemon is running (threshold, rumble filter, mic name, input source, auto level) instead of silently flipping a yellow "Pending Changes" status. The status pill itself only answers whether the virtual mic is running — Active green, Inactive gray.
- Device rows are richer than plain text lines, an empty list shows a real empty state with a Retry scan button, and the voice-test controls keep fixed geometry so nothing moves mid-recording.
- Every control shares the app accent: the buttons, the processing toggles, and the threshold slider are drawn by the app itself rather than the stock widget set, whose light-blue accent is hardcoded upstream. The slider's track now also runs flush with its scale anchors instead of starting half a thumb inside them.
- The minimum window size dropped from 920x800 to 720x680, so the window fits 768p displays.

### Fixed
- Small text is legible again. Labels, captions and helper text sat at 10-11.5px, below the point where the software renderer's unhinted grayscale glyph rasterization keeps stem definition; they now sit on a shared six-step scale in the theme with a 12px floor. The 10.5, 11.5 and 12.5px steps were an illusion regardless — the renderer truncates a requested size to whole physical pixels, so they had been rendering as 10, 11 and 12px all along. The threshold band and the reserved error strip gave up the slack the larger text needed, so the window still fits its stated 720x680 minimum without clipping the footer buttons.
- A voice-test recording now stops at the advertised 15 seconds instead of 16, and if starting the recorder fails the card returns to idle instead of running a phantom countdown that ended in playback rows for a capture that never happened. The Stop button also stays clickable if the mic deactivates mid-recording.
- The footer's unsaved-changes chips and the status pill no longer go stale: typing a mic name, refreshing the device list, or another client (the CLI) changing settings all update the staged-vs-applied diff now. Previously each of those paths silently diverged from what Apply would actually send.
- Clicking Enable with no input source selected reports itself in the error line instead of doing nothing.
- Apply with zero staged changes no longer tears down and recreates the live virtual mic — apps capturing from it used to lose the device for nothing.
- If the daemon dies mid-playback the stop glyphs clear and the reconcile poll returns to its 1-second heartbeat instead of hammering the dead socket at 250 ms with the glyphs stuck lit.
- A PipeWire failure at launch opens the window with the error and an empty, retryable device list instead of exiting before any window appears — a silent death when started from the desktop.
- The help-popup cards size themselves to their wrapped text instead of clipping it when the resolved monospace font wraps a paragraph onto more lines than the hand-tuned heights allowed.
- Assistive technology can no longer activate disabled buttons or move a disabled slider, and the previously unnamed controls — the links, the refresh control, the play/stop tiles, the device rows, the processing toggles, the threshold slider, and the popup close button — now expose proper names, roles, and actions.
- Spawned processes (xdg-open per link click, the auto-spawned daemon) are reaped instead of lingering as zombies.
- Apply and Disable are disabled during a voice-test recording. Both used to tear down the mic node while the recorder was still capturing through it, which silently truncated or stalled the recording.
- Disabling the mic no longer permanently freezes the primary action button's label: an imperative assignment in the old Disable handler destroyed the label's binding the first time it ran.
- The modal overlays now dim the whole window rather than just the central canvas, they track the window size instead of assuming 920x760, and the device-troubleshooting cards scroll instead of clipping their final tip at small sizes.

## [0.7.0] - 2026-07-28

### Added
- Flatpak packaging under `packaging/flatpak`, aimed at Flathub. The build is sandboxed and asks for four permissions: Wayland, X11 as a fallback, IPC, and the PipeWire socket. It needs no GPU access because the GUI renders in software, and no PulseAudio socket because Spectral speaks PipeWire natively.

### Changed
- The interface now uses whichever monospaced font you have configured rather than insisting on JetBrains Mono. Nothing bundles or pins a font any more, and the Nix build no longer overrides your fontconfig setup to force one.

### Fixed
- The interface no longer falls back to a proportional font when JetBrains Mono is absent, which is what happened on every packaging target except Nix. Slint looks up whatever `default-font-family` holds as a font *name*, so neither a missing family nor the CSS generic "monospace" resolves; the preferred monospaced family is now resolved against the system font database before the window opens.
- The virtual mic and the default-input switch now work inside a sandbox. WirePlumber grants a Flatpak client only read and execute permission unless it identifies itself as a graph manager, which Spectral, having to create nodes and links of its own, is. Native installs are unaffected — they were already unrestricted.
- Audio interfaces whose inputs are split into separate sources (many USB interfaces, via the session manager's ALSA splitting) no longer appear as several identically named entries in the device list. Each input is now labeled individually — "EVO4 Mic 1 / Line 1 / Instrument" rather than five indistinguishable "EVO4" rows.

## [0.6.2] - 2026-07-28

### Fixed
- Starting a voice-test recording at the same moment the virtual mic is being enabled no longer fails: the recorder now targets the mic that is actually running rather than the name it is about to be given.
- The real-time capture path reserves its staging buffers in full up front, so a stalled consumer or a large graph quantum can no longer trigger a memory allocation inside the RT audio callback.
- PipeWire registry queries (device listing, enable-time device resolution, default-source configuration) now fail with an error when the PipeWire connection dies mid-query instead of waiting forever — previously this could permanently wedge the daemon and freeze the GUI.
- GUI and CLI requests to the daemon time out after 30 seconds instead of blocking indefinitely when the daemon stops responding.
- A voice-test recording orphaned by its client (e.g. the GUI crashed mid-recording) is now stopped by the daemon after 60 seconds instead of writing WAV data until the disk fills.
- Two daemons spawned at the same instant (for example the GUI and the CLI both auto-starting one) can no longer race each other into a state where a hidden second daemon survives without a socket: an OS-level lock on the pid file now decides ownership.
- Refreshing the device list keeps the selection on the same device even when the list's order or contents changed; previously the selection could silently land on a different device. A selected device that disappeared now clears the selection.
- Settings are saved atomically (temp file plus rename), so a crash or power loss mid-save can no longer corrupt `state.toml`. An unreadable settings file is now reported in the daemon log instead of silently resetting everything to defaults.

## [0.6.1] - 2026-07-18

### Fixed
- The Rename and Reset buttons no longer overlap the virtual-mic name field. The row is a real layout now: the buttons sit beside the field, match the rounded fluent look of every other button with centered labels, and the invalid-name flash border is fully visible around the field instead of disappearing behind them.

### Changed
- The virtual-mic name limit is 20 characters (was 21), sized so a max-length name always fits the name field without scrolling. The CLI and daemon error messages now derive from the shared constant. A previously saved name that exceeds the new limit resets to the default at daemon startup, as before.

## [0.6.0] - 2026-07-17

### Changed
- Voice-test recording and playback are now native PipeWire streams inside the daemon instead of spawned `pw-record`/`pw-play` child processes. This removes two runtime install dependencies (the PipeWire *tools* package) and eliminates the orphan-subprocess and silent-link-failure classes of bugs: a vanished capture target is now a hard error (`node.dont-reconnect`), and Shutdown/SIGTERM tear down the recorder and player cleanly with WAVs finalized instead of leaving `pw-record` running forever. The on-disk WAV format, IPC wire format, and GUI behavior are unchanged.

## [0.5.0] - 2026-07-11

### Added
- A Rumble filter toggle (a GUI switch and `spectral enable --rumble-filter on|off`): a high-pass filter at 90 Hz ahead of the denoiser that cuts desk thumps, handling noise, and HVAC rumble, so the denoiser and its voice detector analyze a cleaner signal. Off by default.
- An Auto level toggle (a GUI switch and `spectral enable --auto-level on|off`): a slow automatic gain rider that evens out mic distance and voice-volume swings toward a −18 dBFS RMS target (at most −12/+18 dB, 3 dB/s), adapting only while you speak so silence is never pumped up, with a built-in −1 dBFS peak limiter as the safety ceiling. Off by default.

### Changed
- The voice gate is rebuilt around how speech actually behaves: a constant 20 ms lookahead opens the gain before a word's first syllable plays (RNNoise's voice detection lags onsets, which no attack speed could fix), a 200 ms hold keeps natural pauses at full volume, hysteresis stops confidence flutter from chattering the gate mid-word, and the closed gate fades to −20 dB of quiet, already-denoised room tone instead of muting to digital silence. The pipeline is 20 ms longer end-to-end (constant, including at threshold 0).
- Auto level now reduces gain four times faster than it raises it (12 versus 3 dB/s), the standard gain-rider convention: a sudden loud passage is tamed in under a second while quiet passages still come up gently.
- The default input threshold is now 50% instead of 80%: the rebuilt gate's lookahead, hold, and hysteresis make the stricter default unnecessary. Fresh installs only — a saved threshold is never touched.
- The Help view is rewritten: dedicated Rumble Filter and Auto Level sections explain each stage and when to enable it, the Input Threshold section gains a slider-scale diagram, and the "What is Spectral?" intro and status-color tip are removed.

### Fixed
- Devices with more than two capture channels now downmix only the first two. USB interfaces exposing surround capture profiles (for example the Audient EVO4's 4-channel mode) put the physical preamps on the front pair and loopback/monitor returns on the rest; averaging all channels attenuated the mic by up to 12 dB, mixed in an unused preamp's noise floor, and could bleed system audio into the virtual mic.
- The voice gate's gain now ramps sample-by-sample within each frame instead of stepping once per 10 ms frame, removing a faint zipper on gate transitions.

### Internal
- The IPC protocol version is now 2 (the daemon state and enable command carry the new toggles), so clients detect and replace a running 0.4.x daemon on first use.

## [0.4.0] - 2026-07-08

### Added
- Headless CLI: `spectral enable|disable|status|devices|shutdown` control the daemon without the GUI (bare `spectral` still opens it). `enable` accepts `--device`, `--threshold`, and `--mic-name`, spawns the daemon when none is running, and keeps the current settings for omitted flags. Exit codes: 0 success, 1 runtime failure, 2 usage error.
- AUR packaging under `packaging/aur/` (package name `spectral-mic`; the AUR name `spectral` belongs to an unrelated project shipping the same binary name, so the package conflicts with it).
- AppStream metainfo, installed to `share/metainfo`, so software centers can index the app.
- `THIRD-PARTY.md` reproducing nnnoiseless's BSD-3-Clause notice (including the bundled RNNoise model weights), installed to `share/licenses`.
- Cargo package metadata (homepage, keywords, categories) and a tuned release profile: thin LTO, one codegen unit, stripped symbols; panic stays `unwind`.

### Changed
- The daemon remembers the selected input device by PipeWire node name and re-resolves it when enabling, so re-enabling survives reboots and PipeWire restarts (numeric device ids are scoped to one PipeWire instance and go stale with it).
- The Help and device-troubleshooting popups now describe the PipeWire-native reality: WirePlumber in the service check, `wpctl status` and Bluetooth HSP/HFP profiles in diagnosis, guidance to Refresh after a PipeWire restart (device entries go stale), honest staged-vs-applied rename semantics, and a Getting Started that reflects the automatic default-input switch instead of a hardcoded mic name.
- README overhauled: the stale PulseAudio (libpulse) requirement is gone, AUR install instructions added, and new troubleshooting, known-limitations, and non-goals sections. A follow-up pass added a rendered table of contents, collapsed the per-distro install instructions, and dropped the renderer notes (self-documented in the flake and source). A header pass centered the project icon with CI and license badges beneath it; the table of contents is now hand-written, because the frontmatter-generated one always renders above all other content.
- The desktop entry is now a committed file installed by the Nix package (previously generated at build time), so all packaging shares one source of truth.

### Fixed
- Clients now reliably detect and replace an outdated running daemon after an upgrade, via an explicit protocol-version field in the daemon's state. The previous detection relied on newly added state fields failing to parse — which optional fields never do — so a stale daemon could survive an upgrade undetected and fail later requests confusingly.

### Internal
- Every push now runs `nix flake check` (build + tests, clippy with denied warnings, formatting) on Codeberg via Forgejo Actions on a self-hosted runner, with a status badge in the README.
- The flake's apps carry `meta` descriptions, so flake evaluation (locally and in every CI log) is warning-clean.
- Removed the `spectral-teardown` dev-shell helper: `spectral shutdown` stops the daemon, the native PipeWire mic dies with the process, and the daemon already recovers from stale socket and pid files on its next start.

## [0.3.0] - 2026-07-08

### Added
- The window now shows an error line under the status bar when an action fails, including when the daemon cannot be reached. Previously errors went to stderr only, which a desktop launch never shows.
- Launching Spectral without a reachable daemon now opens the window with an explanation instead of exiting before any window appeared.

### Changed
- The virtual microphone is now a native PipeWire source owned by the daemon instead of PulseAudio null-sink + remap-source modules. Exactly one new device appears — an input; the "Spectral-DoNotUse" output device is gone — and the mic vanishes the instant the daemon exits, so leaked devices are impossible by construction. The default input is selected through PipeWire metadata (the same value `wpctl set-default` writes), and the pipeline now requests a 10 ms quantum, cutting end-to-end latency when the graph grants it.
- The input threshold now spans the whole slider: 0% is the explicit denoise-only baseline (voice gate off) and 1–100% map linearly to a VAD confidence cutoff of up to 0.95. Previously 0–49% were all identical to denoise-only and only 50–100% drove the gate. Values 50–100% keep almost exactly their old meaning (the default of 80% moves from a 0.77 to a 0.76 cutoff); values 1–49% go from doing nothing to a lenient gate.

### Removed
- The `--reap` subcommand and all PulseAudio module handling, including the libpulse dependency — with the mic's objects owned by the daemon process there is nothing left to leak or reap. One-time note when upgrading from 0.2.x: modules leaked by old versions are no longer swept automatically. Any 0.2.3 daemon start already cleaned them; a lingering "Spectral-DoNotUse"/"Spectral-Mic" pair can be removed with `pactl unload-module module-remap-source` then `pactl unload-module module-null-sink`, or a session restart.

### Fixed
- The daemon now detects when the audio pipeline dies (for example after a PipeWire restart), tears down the dead virtual mic instead of leaving a silent zombie device selectable, and reports the reason. Previously the status stayed Active while the mic produced silence.
- The GUI now reconciles its status display with the daemon every second instead of trusting its last click, so a pipeline or daemon death shows up within about a second. Voice-test playback still polls at 250 ms.
- Disabling now reports virtual-mic teardown failures (the leftover modules are reaped on the next enable) instead of always claiming success.
- Out-of-range threshold values from a hand-edited settings file or a rogue socket client are now clamped to 0–100; previously they produced a cutoff above 1.0 that the voice detector can never reach — a permanently silent virtual mic.
- The Help view's threshold entry described the slider as adjusting the noise-suppression strength; it now explains the actual voice-gate mechanism (per-frame VAD confidence against a cutoff).
- Starting a voice test no longer leaks the previous recording's `pw-record` processes. Previously a start without an intervening stop (for example after relaunching the GUI mid-recording) orphaned the old pair, which kept running and kept writing to the same output files as the new pair; partial spawn failures could likewise orphan the recorder that did start.

### Internal
- The real-time audio path no longer takes locks or allocates: the threshold is an atomic, the denoiser state is owned by the pipeline thread, and both stream callbacks reuse preallocated buffers.
- Node names are resolved from the PipeWire registry instead of shelling out to `wpctl`, dropping the WirePlumber CLI from the daemon's runtime dependencies.
- Enable's readiness gate now verifies both streams and the mic's feed link natively; the previous gate only waited for the PulseAudio source to register.
- The daemon's state query now takes its locks one at a time instead of holding all three simultaneously in reverse of the documented lock order, closing a latent deadlock trap.

## [0.2.3] - 2026-07-06

### Changed
- Locating the daemon socket now requires `XDG_RUNTIME_DIR` and fails with a clear error when it is unset. The previous `/tmp/runtime-$USER` fallback pointed at a directory that was never created, so connecting there could never succeed.

### Fixed
- Startup and enable no longer rely on fixed sleeps: the GUI retries the daemon connection with a bounded deadline after spawning it, waits for an old daemon to actually exit before respawning one, and the daemon polls PulseAudio until the virtual source is registered before selecting it as the default input.
- Voice-test recordings now live under `~/.local/state/spectral/recordings` (the XDG state directory) instead of `~/.config/spectral/recordings`, keeping WAV audio out of config-directory backups. The two old files are swept automatically on daemon startup.

### Internal
- The UI derives control enablement from an explicit `mic_enabled` property instead of comparing the status color against red.
- The dev shell consumes the package's runtime library list via `passthru` instead of maintaining a duplicate copy in the flake.
- `nix flake check` now actually gates: it builds the package (which runs the test suite), lints with clippy under denied warnings, and verifies formatting.
- Removed the PulseAudio connect success print (error-only logging convention), a stale cross-reference comment, and the Windows `USERPROFILE` fallback from the Linux-only persistence module.

## [0.2.2] - 2026-06-30

### Fixed
- The running application window now shows the Spectral icon in the Wayland taskbar/dock instead of GNOME's generic fallback glyph. The XDG app id is now set after the window's platform context is initialized — previously the call ran before any Slint context existed, silently failed, and never reached the compositor, so GNOME could not match the live window to its desktop file. The app id, desktop file, `Icon=`, and `StartupWMClass` now use the reverse-DNS name `page.codeberg.mmc.Spectral`. (#1)

## [0.2.1] - 2026-06-30

### Changed
- Updated the project icon.

### Internal
- Removed dead code left over from earlier feature removals: the inert signal-handler mpsc channel, the uncalled `sanitize_mic_name` helper, a no-op `SPECTRAL_RENDERER_ATTEMPT` environment cleanup, and the unread `is_recording`/`has_recording` fields from the daemon IPC state.
- Removed the placeholder `macos`/`windows` platform module declarations (Spectral is Linux-only), which restores the `cargo fmt --check` gate.

## [0.2.0] - 2026-06-29

### Added
- `spectral-daemon --reap` subcommand, plus automatic cleanup of leaked virtual-mic modules at daemon startup, before each enable, and from `spectral-teardown`.
- Mic-name input validation with live keystroke rejection in the GUI.

### Changed
- Enabling now fails with a clear error when the audio pipeline cannot start, instead of presenting an active-but-silent virtual microphone.
- Settings are persisted when you click Enable; the unused active-state restore was removed and the daemon always starts inactive.
- The daemon recovers from a poisoned lock instead of crashing, for consistent behavior across its threads.
- Pinned Slint to 1.15.0 using the software renderer only.

### Fixed
- Duplicate and stale "Spectral-DoNotUse" outputs / "Spectral-Mic" inputs no longer accumulate in the system audio settings — leaked PulseAudio modules are reaped.
- Starting a recording with no input device selected now returns a clear error instead of silently dropping the connection.

### Documentation
- README: removed the Development section and corrected the Usage steps (the GUI starts the daemon automatically).

### Internal
- Removed dead IPC commands and pending-state machinery; deduplicated the wpctl node lookup, virtual-mic node naming, the GUI Active/Pending recompute, and the PulseAudio wait-with-timeout pattern; added the `spectral-teardown` dev helper.

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

[Unreleased]: https://codeberg.org/mmc/spectral/compare/v0.9.0...main
[0.9.0]: https://codeberg.org/mmc/spectral/compare/v0.8.0...v0.9.0
[0.8.0]: https://codeberg.org/mmc/spectral/compare/v0.7.0...v0.8.0
[0.7.0]: https://codeberg.org/mmc/spectral/compare/v0.6.2...v0.7.0
[0.6.2]: https://codeberg.org/mmc/spectral/compare/v0.6.1...v0.6.2
[0.6.1]: https://codeberg.org/mmc/spectral/compare/v0.6.0...v0.6.1
[0.6.0]: https://codeberg.org/mmc/spectral/compare/v0.5.0...v0.6.0
[0.5.0]: https://codeberg.org/mmc/spectral/compare/v0.4.0...v0.5.0
[0.4.0]: https://codeberg.org/mmc/spectral/compare/v0.3.0...v0.4.0
[0.3.0]: https://codeberg.org/mmc/spectral/compare/v0.2.3...v0.3.0
[0.2.3]: https://codeberg.org/mmc/spectral/compare/v0.2.2...v0.2.3
[0.2.2]: https://codeberg.org/mmc/spectral/compare/v0.2.1...v0.2.2
[0.2.1]: https://codeberg.org/mmc/spectral/compare/v0.2.0...v0.2.1
[0.2.0]: https://codeberg.org/mmc/spectral/compare/v0.1.0...v0.2.0
[0.1.0]: https://codeberg.org/mmc/spectral/releases/tag/v0.1.0
