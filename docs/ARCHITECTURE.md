# Architecture

A map of the codebase for people changing it. For building, testing, and
submitting patches, see [CONTRIBUTING.md](../CONTRIBUTING.md).

## Process model

One crate produces two binaries plus a shared library:

| Component | Entry point | Role |
| --- | --- | --- |
| `spectral` | `src/main.rs` | Slint GUI control panel, and the headless CLI when given a subcommand. A thin client — it does no audio work. |
| `spectral-daemon` | `src/bin/spectral-daemon.rs` | Long-lived backend. Owns all audio state and the virtual mic, persists settings, serves clients. |
| `spectral_lib` | `src/lib.rs` | Shared code: state types, IPC, and the platform layer. |

The daemon is the single owner of audio state. Clients connect over a Unix
socket at `$XDG_RUNTIME_DIR/spectral-daemon.sock`, and the daemon serves one
handler thread per connection. Either client auto-spawns the daemon if none is
running; a lock on `$XDG_RUNTIME_DIR/spectral-daemon.pid` decides ownership when
two try at once. Daemon diagnostics go to `$XDG_RUNTIME_DIR/spectral-daemon.log`.

Because the daemon owns every PipeWire object it creates, the virtual mic
disappears when the daemon exits. There is nothing to reap and no state left in
the audio graph.

## Audio path

The pipeline runs on a dedicated thread with its own PipeWire mainloop
(`NativeAudioPipeline`, `src/platform/linux/pipewire.rs`). A capture stream
targets the selected input device; an explicitly created link feeds a
daemon-owned `Audio/Source/Virtual` node — the microphone other applications
see.

Per 480-sample frame (10 ms at 48 kHz — the size RNNoise requires):

1. **Downmix to mono** and scale into the i16-range floats RNNoise expects
   (`RNNOISE_SCALE_FACTOR`, `pipewire.rs`).
2. **High-pass** at 90 Hz — the rumble filter, off by default. A 2nd-order
   Butterworth biquad, applied *before* the denoiser so RNNoise and its VAD
   analyze a rumble-free signal (`stages/highpass.rs`).
3. **RNNoise denoise**, which also returns the frame's voice-activity
   probability (0.0–1.0). That probability drives both stages below.
4. **Leveler** — auto level, off by default. A slow RMS-tracking gain rider
   toward −18 dBFS with a peak limiter, frozen on non-voice frames so noise
   floors are never pumped up (`stages/leveler.rs`).
5. **Voice gate**, always in the chain. Turns the VAD probability into a
   click-free gain using 20 ms of lookahead, hysteresis, a 200 ms hold, and a
   −20 dB floor rather than a hard mute (`stages/gate.rs`).

`AudioProcessor::process_frame` (`src/platform/linux/audio_processor.rs`)
composes these in that order and is the authoritative reference. It reads the
user's settings from atomics on `AppState` every frame, so toggling a stage
applies live without ever locking the audio path.

Two latency contributions are fixed and independent of settings: RNNoise's
10 ms algorithmic delay, and the gate's 20 ms lookahead. The gate's delay is
constant even when the gate is bypassed, so latency never shifts as the user
changes the threshold. Everything else depends on the PipeWire graph quantum,
which any client on the system can change.

The UI threshold maps to a VAD confidence cutoff via `vad_cutoff`: 0 disables
the gate entirely (denoise only), and 1–100 map linearly onto a cutoff up to
0.95.

## Module map

| Path | Responsibility |
| --- | --- |
| `src/state.rs` | `AppState` (the daemon's shared state), `AudioDevice`, the `Status` lifecycle enum, mic-name validation, and `lock_or_recover`. |
| `src/main.rs` | Slint GUI. UI in `ui/`, where `app-window.slint` composes the app bar, threshold band, cards, footer and popups against the shared `theme.slint`; compiled by `build.rs` and bound via `slint::include_modules!()`. |
| `src/cli.rs` | Headless subcommand parsing and execution. Bare `spectral` launches the GUI. |
| `src/fonts.rs` | Resolves the user's preferred monospaced family, which Slint cannot ask for itself. |
| `src/daemon_launch.rs` | Spawning the daemon and bootstrapping a connection; shared by the GUI and CLI. |
| `src/diagnostics.rs` | Renders the bug-report dump behind the GUI's error details modal, from in-process state only. |
| `src/bin/spectral-daemon.rs` | Daemon main loop, connection handlers, and the lock-ordering convention. |
| `src/platform/mod.rs` | The platform seam; re-exports `linux`. |
| `src/platform/linux/pipewire.rs` | `NativeAudioPipeline`: the audio thread, capture stream, virtual source node, and link. |
| `src/platform/linux/audio_processor.rs` | `AudioProcessor` (the per-frame chain) and `VirtualMic` (lifecycle of the mic). |
| `src/platform/linux/stages/` | The individual DSP stages: `highpass.rs`, `leveler.rs`, `gate.rs`. |
| `src/platform/linux/audio_devices.rs` | PipeWire registry queries: enumerate inputs, resolve node names, set the default source. |
| `src/platform/linux/ipc.rs` | Unix-socket JSON IPC: `DaemonServer`, `DaemonClient`, commands, state. |
| `src/platform/linux/persistence.rs` | XDG TOML persistence and the on-disk recording locations. |
| `src/platform/linux/recorder.rs` | Native PipeWire recorder and player for the voice test. |

Settings persist to `$XDG_CONFIG_HOME/spectral/state.toml`; voice-test
recordings live under the XDG state dir at `~/.local/state/spectral/recordings`.

## Threading and real-time constraints

The real-time audio path takes **no locks and makes no allocations**. Stages
allocate their buffers in `new` and reuse them; settings reach the audio thread
through atomics. Any change to `process_frame` or a stage's `process` must
preserve this.

Handle structs that own a thread are named `Native*` and follow one lifecycle
(`NativeRecorder` is the reference implementation): a `running: Arc<AtomicBool>`
flag, a `failure: Arc<Mutex<Option<String>>>` slot, and an
`Option<JoinHandle<()>>`. `start()` blocks on an mpsc ready-handshake so callers
know the thread is live; `stop()` clears the flag and joins; `Drop` calls
`stop()`. Thread bodies are named `run_*`.

Off the audio path, the daemon guards optional subsystems with
`Arc<Mutex<Option<T>>>`. Inside the single-threaded PipeWire callbacks, shared
state is `Rc<RefCell<…>>`. All lock acquisition goes through `lock_or_recover`
(`src/state.rs`), which recovers from poisoning rather than panicking — a
long-lived daemon prefers stale data to a crash, especially where a panic would
cross the PipeWire FFI boundary.

**Locks must be acquired in the documented order.** It is written out in the
`LOCK ORDERING CONVENTION` comment in `src/bin/spectral-daemon.rs`; read it
before adding a lock acquisition, and keep guard scopes minimal.

## IPC protocol

JSON request/response, one message per line, defined in
`src/platform/linux/ipc.rs`. A `DaemonCommand` (`Enable`, `Disable`,
`GetState`, `Shutdown`, and the voice-test recording and playback commands)
gets back a `DaemonResponse` — `Ok`, `State(DaemonState)`, or `Error(String)`.
Errors cross the socket stringified.

`Enable` carries every setting as an `Option`; `None` means "keep what the
daemon already has". Device ids are scoped to one PipeWire server instance, so
the daemon also remembers the device's `node.name` and re-resolves from it,
which is what makes a flagless `spectral enable` work across reboots.

`PROTOCOL_VERSION` guards client/daemon mismatches. Bump it when you change the
wire format incompatibly. Clients check it on connect and restart a daemon that
answers with an older version, so an upgraded client is never left talking to a
stale daemon left running from a previous install
(`connect_and_fetch_state`, `src/daemon_launch.rs`).

The GUI holds no authoritative state of its own. It polls the daemon on a 1 s
heartbeat (250 ms during voice-test playback) and reconciles status and errors
from the snapshot it gets back.

Because that socket is opened per command and dropped again, the daemon has no
disconnect to act on: a closed window looks exactly like an idle one. So the GUI
sends the voice-test stops it still owes — `StopRecording`, `StopOriginal`,
`StopFiltered` — after its event loop exits
(`stop_playback_and_recording_on_exit`, `src/main.rs`). This is best effort; a
killed process sends nothing, which is
what the recorder's own duration cap backstops. The virtual mic deliberately
survives the window and is never torn down here.

## Where to make common changes

**Adding a DSP stage.** Write it as a struct in
`src/platform/linux/stages/`, following the shape of `highpass.rs`: allocate in
`new`, expose `process` and `reset`, allocate nothing in `process`. Export it
from `stages/mod.rs`, then compose it into `AudioProcessor::process_frame` in
the right position — order matters, and the module doc at the top of
`audio_processor.rs` records the current chain. If it is user-toggleable, it
needs an atomic on `AppState`, a field in `PersistedState`, an `Option` on
`DaemonCommand::Enable`, a CLI flag, and a Slint switch.

**Adding a CLI subcommand.** Extend `CliCommand` and the parser in
`src/cli.rs`, add the matching `DaemonCommand` variant in `ipc.rs`, and handle
it in the daemon's connection loop in `src/bin/spectral-daemon.rs`. A wire
format change means bumping `PROTOCOL_VERSION`.

**Adding a persisted setting.** Add the field to `PersistedState` in
`persistence.rs` with `#[serde(default)]` so that files written by older
versions still load — this is a hard requirement, not a style preference — then
thread it through `AppState`, the IPC surface, and whichever clients expose it.

**Changing the DSP.** Listening validation is required; the audible result is
the specification. Unit tests cover the arithmetic, not the sound.

## Testing

Tests are inline `#[cfg(test)] mod tests` blocks. They cover pure logic only —
nothing touching PipeWire or sockets — so that the whole suite runs in the nix
sandbox. `nix flake check` builds the package and runs formatting, clippy (all
warnings denied), and the tests.
