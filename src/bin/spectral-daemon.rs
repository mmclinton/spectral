//! The long-lived backend: owns the virtual mic and all audio state,
//! persists settings, and serves GUI/CLI clients on the Unix socket with
//! one handler thread per connection. SIGTERM/SIGINT tear everything down.
//!
//! LOCK ORDERING CONVENTION: to prevent deadlocks, always acquire locks in
//! this order (the atomics — `state.threshold_val`, `state.rumble_filter`,
//! `state.auto_level` — are exempt):
//!
//! 1. `state.mic_name`
//! 2. `state.selected_device_id`
//! 3. `state.device_node_name`
//! 4. `virtual_mic`
//! 5. `state.status`
//! 6. `state.last_error`
//! 7. `recorder`
//! 8. `player`
//!
//! Keep guard scopes minimal; never hold two locks longer than necessary.

use spectral_lib::platform::audio_devices::{find_node, get_node_details};
use spectral_lib::platform::audio_processor::VirtualMic;
use spectral_lib::platform::ipc::{
    DaemonCommand, DaemonResponse, DaemonServer, DaemonState, PROTOCOL_VERSION, daemon_log_path,
    wake_accept_loop,
};
use spectral_lib::platform::persistence::{
    PersistedState, get_filtered_recording_path, get_original_recording_path, load_state,
    save_state,
};
use spectral_lib::platform::recorder::{NativePlayer, NativeRecorder, PlaybackKind};
use spectral_lib::state::{AppState, Status, is_valid_mic_name, lock_or_recover, mic_name_rule};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// Drop whatever a previous session left in the log, now that the pid lock
/// says this one owns it.
///
/// Truncating the inode is safe while our own stdout and stderr still point
/// at it: clients open it with `O_APPEND`, so every write recomputes its
/// offset from the end rather than from a stale one. Best effort — a log we
/// cannot start clean is not a reason to refuse to run.
fn start_log_afresh() {
    let Ok(path) = daemon_log_path() else { return };
    match std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&path)
    {
        Ok(_) => {}
        // No log file means nobody spawned us through a client — started by
        // hand or by a service manager, with output already going elsewhere.
        // Nothing to start afresh, and deliberately not created here: an
        // empty file nothing writes to would just litter the runtime dir.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => eprintln!("[daemon] could not start the log afresh: {e}"),
    }
}

fn main() {
    let state = Arc::new(AppState::new());
    let virtual_mic: Arc<Mutex<Option<VirtualMic>>> = Arc::new(Mutex::new(None));

    let recorder: Arc<Mutex<Option<NativeRecorder>>> = Arc::new(Mutex::new(None));
    let player: Arc<Mutex<Option<(PlaybackKind, NativePlayer)>>> = Arc::new(Mutex::new(None));

    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();

    let mut signals = signal_hook::iterator::Signals::new([
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGINT,
    ])
    .expect("Failed to register signal handlers");

    let signal_handle = signals.handle();

    let signal_thread = thread::spawn(move || {
        if signals.forever().next().is_some() {
            r.store(false, Ordering::SeqCst);
            // The accept loop is blocked on a client that may never come;
            // clearing the flag alone would leave it there until one does.
            wake_accept_loop();
        }
    });

    // Claim ownership before doing any work: a daemon that loses the pid
    // lock has nothing to contribute, and everything below this point wants
    // to be logged into a file this session owns.
    let server = match DaemonServer::bind() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[daemon] failed to bind socket: {e}");
            return;
        }
    };
    start_log_afresh();

    let mut persisted = match load_state() {
        Ok(persisted) => persisted,
        Err(e) => {
            eprintln!("[daemon] failed to load persisted settings, using defaults: {e}");
            PersistedState::default()
        }
    };
    if !is_valid_mic_name(&persisted.mic_name) {
        let invalid = &persisted.mic_name;
        eprintln!("[daemon] persisted mic_name {invalid:?} is invalid, resetting to default");
        persisted.mic_name = "Spectral-Mic".to_string();
    }
    if !(0..=100).contains(&persisted.threshold) {
        let threshold = persisted.threshold;
        eprintln!("[daemon] persisted threshold {threshold} is out of range, clamping");
        persisted.threshold = persisted.threshold.clamp(0, 100);
    }
    {
        state
            .threshold_val
            .store(persisted.threshold, Ordering::Relaxed);
        state
            .rumble_filter
            .store(persisted.rumble_filter, Ordering::Relaxed);
        state
            .auto_level
            .store(persisted.auto_level, Ordering::Relaxed);
        *lock_or_recover(&state.mic_name) = persisted.mic_name.clone();
        *lock_or_recover(&state.selected_device_id) = persisted.device_id;
        *lock_or_recover(&state.device_node_name) = persisted.device_node_name.clone();
    }

    let mut client_threads: Vec<thread::JoinHandle<()>> = Vec::new();

    loop {
        if !running.load(Ordering::SeqCst) {
            break;
        }

        client_threads.retain(|handle| !handle.is_finished());

        match server.accept() {
            Ok(mut conn) => {
                // A shutdown wake arrives as an ordinary connection; serving
                // it would only spawn a thread to read an immediate EOF.
                if !running.load(Ordering::SeqCst) {
                    break;
                }

                let state = state.clone();
                let virtual_mic = virtual_mic.clone();
                let recorder = recorder.clone();
                let player = player.clone();

                let handle = thread::spawn(move || {
                    if let Err(e) = handle_client(&mut conn, state, virtual_mic, recorder, player)
                        && e.kind() != std::io::ErrorKind::WouldBlock
                        && e.kind() != std::io::ErrorKind::TimedOut
                    {
                        eprintln!("[daemon] client handler error: {e}");
                    }
                });

                client_threads.push(handle);
            }
            // EINTR is transient and never how shutdown arrives — that comes
            // as the wake connection above — so retry it silently. Anything
            // else is worth reporting, and the short pause keeps a
            // persistently failing accept from spinning the CPU.
            Err(e) => {
                if e.kind() != std::io::ErrorKind::Interrupted {
                    eprintln!("[daemon] failed to accept connection: {e}");
                    thread::sleep(Duration::from_millis(100));
                }
            }
        }
    }

    if let Some(mic) = lock_or_recover(&virtual_mic).take() {
        mic.stop();
    }

    for handle in client_threads {
        let _ = handle.join();
    }

    signal_handle.close();

    let _ = signal_thread.join();
}

/// Stop and tear down the player slot iff it currently holds `kind`. A
/// no-op when the slot is empty or holds the other kind. The slot's Drop
/// would stop the thread anyway, but taking it here joins eagerly so the
/// audio thread is gone before the next command runs.
fn stop_player_of_kind(player: &Mutex<Option<(PlaybackKind, NativePlayer)>>, kind: PlaybackKind) {
    let mut guard = lock_or_recover(player);
    if let Some((k, mut p)) = guard.take() {
        if k == kind {
            p.stop();
        } else {
            *guard = Some((k, p));
        }
    }
}

/// An Enable command's settings; `None` means "keep the current setting".
struct EnableRequest {
    device_id: Option<u32>,
    device_node_name: Option<String>,
    mic_name: Option<String>,
    threshold: Option<i32>,
    rumble_filter: Option<bool>,
    auto_level: Option<bool>,
}

/// Which handle `handle_enable` should resolve the capture device from.
#[derive(Debug, PartialEq, Eq)]
enum DeviceLookup {
    /// A durable `node.name`, from the client or from what we remember.
    ByName(String),
    /// A PipeWire id supplied by the client; valid only this instance.
    ById(u32),
    /// Nothing to go on.
    NoDevice,
}

/// A supplied name wins over a supplied id because ids die on replug and
/// on a PipeWire restart while names do not, and a client that sends both
/// is describing one device. The id branch stays for `--device <id>`,
/// which is the one place a user names a device by id on purpose.
fn plan_device_lookup(
    requested_name: Option<String>,
    requested_id: Option<u32>,
    remembered_name: Option<String>,
) -> DeviceLookup {
    match (requested_name, requested_id, remembered_name) {
        (Some(name), _, _) => DeviceLookup::ByName(name),
        (None, Some(id), _) => DeviceLookup::ById(id),
        (None, None, Some(name)) => DeviceLookup::ByName(name),
        (None, None, None) => DeviceLookup::NoDevice,
    }
}

/// Resolve the effective settings and device for an Enable request, then
/// (re)create the virtual mic. The running mic is only stopped once
/// resolution has fully succeeded, so a rejected request leaves the active
/// state untouched.
fn handle_enable(
    state: &Arc<AppState>,
    virtual_mic: &Mutex<Option<VirtualMic>>,
    request: EnableRequest,
) -> DaemonResponse {
    let EnableRequest {
        device_id,
        device_node_name,
        mic_name,
        threshold,
        rumble_filter,
        auto_level,
    } = request;
    let mic_name = match mic_name {
        Some(name) => {
            if !is_valid_mic_name(&name) {
                let rule = mic_name_rule();
                return DaemonResponse::Error(format!("Invalid mic name: {rule}"));
            }
            name
        }
        None => lock_or_recover(&state.mic_name).clone(),
    };
    let threshold = match threshold {
        Some(t) => t.clamp(0, 100),
        None => state.threshold_val.load(Ordering::Relaxed),
    };
    let rumble_filter =
        rumble_filter.unwrap_or_else(|| state.rumble_filter.load(Ordering::Relaxed));
    let auto_level = auto_level.unwrap_or_else(|| state.auto_level.load(Ordering::Relaxed));
    let remembered = lock_or_recover(&state.device_node_name).clone();
    let (device_id, device_node_name, channels) = match plan_device_lookup(
        device_node_name,
        device_id,
        remembered,
    ) {
        DeviceLookup::ByName(name) => match find_node(&name) {
            Ok(Some((id, channels))) => (id, name, channels),
            Ok(None) => {
                return DaemonResponse::Error(format!(
                    "Device \"{name}\" is not present — plug it in, or run `spectral devices` and pass --device"
                ));
            }
            Err(e) => {
                return DaemonResponse::Error(format!("Failed to query PipeWire: {e}"));
            }
        },
        DeviceLookup::ById(id) => match get_node_details(id) {
            Ok((name, channels)) => (id, name, channels),
            Err(e) => {
                return DaemonResponse::Error(format!(
                    "Cannot use device {id}: {e} (stale id? refresh the device list or run `spectral devices`)"
                ));
            }
        },
        DeviceLookup::NoDevice => {
            return DaemonResponse::Error(
                "No device selected — pass --device <id> (see `spectral devices`)".to_string(),
            );
        }
    };

    state.threshold_val.store(threshold, Ordering::Relaxed);
    state.rumble_filter.store(rumble_filter, Ordering::Relaxed);
    state.auto_level.store(auto_level, Ordering::Relaxed);
    *lock_or_recover(&state.mic_name) = mic_name.clone();
    *lock_or_recover(&state.selected_device_id) = Some(device_id);
    *lock_or_recover(&state.device_node_name) = Some(device_node_name.clone());

    if let Err(e) = save_state(&PersistedState {
        device_id: Some(device_id),
        device_node_name: Some(device_node_name.clone()),
        mic_name: mic_name.clone(),
        threshold,
        rumble_filter,
        auto_level,
    }) {
        eprintln!("[daemon] failed to persist settings: {e}");
    }

    let mut mic_guard = lock_or_recover(virtual_mic);
    if let Some(mic) = mic_guard.take() {
        mic.stop();
    }

    match VirtualMic::create(state.clone(), &device_node_name, &mic_name, channels) {
        Ok(mic) => {
            *mic_guard = Some(mic);
            state.update_status(Status::Active);
            state.set_last_error(None);
            DaemonResponse::Ok
        }
        Err(e) => {
            state.update_status(Status::Inactive);
            let msg = format!("Failed to create virtual mic: {e}");
            state.set_last_error(Some(msg.clone()));
            DaemonResponse::Error(msg)
        }
    }
}

fn handle_client(
    conn: &mut spectral_lib::platform::ipc::DaemonConnection,
    state: Arc<AppState>,
    virtual_mic: Arc<Mutex<Option<VirtualMic>>>,
    recorder: Arc<Mutex<Option<NativeRecorder>>>,
    player: Arc<Mutex<Option<(PlaybackKind, NativePlayer)>>>,
) -> Result<(), std::io::Error> {
    loop {
        match conn.read_command() {
            Ok(command) => {
                let response = match command {
                    DaemonCommand::Enable {
                        device_id,
                        device_node_name,
                        mic_name,
                        threshold,
                        rumble_filter,
                        auto_level,
                    } => handle_enable(
                        &state,
                        &virtual_mic,
                        EnableRequest {
                            device_id,
                            device_node_name,
                            mic_name,
                            threshold,
                            rumble_filter,
                            auto_level,
                        },
                    ),
                    DaemonCommand::Disable => {
                        let mut mic_guard = lock_or_recover(&virtual_mic);
                        if let Some(mic) = mic_guard.take() {
                            mic.stop();
                        }
                        state.update_status(Status::Inactive);
                        state.set_last_error(None);
                        DaemonResponse::Ok
                    }
                    DaemonCommand::GetState => {
                        // Lazy failure detection: the pipeline only records why it
                        // died; reads reconcile, tearing down the dead mic so no
                        // silent zombie source stays selectable.
                        let (dead_mic, capture_detached) = {
                            let mut mic_guard = lock_or_recover(&virtual_mic);
                            match mic_guard.as_ref().and_then(VirtualMic::failure) {
                                Some(msg) => (mic_guard.take().map(|mic| (mic, msg)), false),
                                None => (
                                    None,
                                    mic_guard.as_ref().is_some_and(VirtualMic::is_detached),
                                ),
                            }
                        };
                        if let Some((mic, msg)) = dead_mic {
                            mic.stop();
                            state.update_status(Status::Inactive);
                            state.set_last_error(Some(format!("Virtual mic stopped: {msg}")));
                        }

                        let status = *lock_or_recover(&state.status);

                        // Reap player liveness: a finished player is torn
                        // down and its failure (if any) surfaces into
                        // last_error, mirroring the mic dead-detection
                        // above. The message is only recorded once the
                        // guard is dropped: last_error (#6) precedes the
                        // player (#8) in the lock order, so it must never
                        // be acquired while #8 is held.
                        let mut is_playing_original = false;
                        let mut is_playing_filtered = false;
                        let player_error = {
                            let mut guard = lock_or_recover(&player);
                            let mut error = None;
                            if let Some((kind, p)) = guard.as_ref() {
                                if p.is_finished() {
                                    error =
                                        p.failure().map(|msg| format!("Playback stopped: {msg}"));
                                    if let Some((_, mut finished)) = guard.take() {
                                        finished.stop();
                                    }
                                } else {
                                    match kind {
                                        PlaybackKind::Original => is_playing_original = true,
                                        PlaybackKind::Filtered => is_playing_filtered = true,
                                    }
                                }
                            }
                            error
                        };
                        if let Some(msg) = player_error {
                            state.set_last_error(Some(msg));
                        }

                        // Reap recorder failures into last_error for
                        // visibility. Recorders are not surfaced in
                        // DaemonState (no recording-active field exists
                        // today, and adding one is a wire-format change),
                        // but a failed recording still deserves to be torn
                        // down and reported. Same ordering discipline:
                        // record after dropping the recorder guard (#7 is
                        // held, #6 must not be).
                        let (recorder_error, is_recording) = {
                            let mut guard = lock_or_recover(&recorder);
                            if let Some(r) = guard.as_ref()
                                && let Some(msg) = r.failure()
                            {
                                if let Some(mut finished) = guard.take() {
                                    finished.stop();
                                }
                                (Some(format!("Recording stopped: {msg}")), false)
                            } else {
                                (None, guard.is_some())
                            }
                        };
                        if let Some(msg) = recorder_error {
                            state.set_last_error(Some(msg));
                        }

                        let threshold = state.threshold_val.load(Ordering::Relaxed);
                        let rumble_filter = state.rumble_filter.load(Ordering::Relaxed);
                        let auto_level = state.auto_level.load(Ordering::Relaxed);
                        let mic_name = lock_or_recover(&state.mic_name).clone();
                        let device_id = *lock_or_recover(&state.selected_device_id);
                        let device_node_name = lock_or_recover(&state.device_node_name).clone();
                        let last_error = lock_or_recover(&state.last_error).clone();
                        let daemon_state = DaemonState {
                            protocol_version: PROTOCOL_VERSION,
                            status,
                            device_id,
                            device_node_name,
                            mic_name,
                            threshold,
                            rumble_filter,
                            auto_level,
                            capture_detached,
                            is_recording,
                            is_playing_original,
                            is_playing_filtered,
                            last_error,
                        };
                        DaemonResponse::State(daemon_state)
                    }
                    DaemonCommand::StartRecording => {
                        // #3 before #4, and #4 exactly once: both mic checks
                        // read the same guard, so another client's Enable
                        // cannot swap the mic between the detached check and
                        // the liveness check.
                        let source_node = lock_or_recover(&state.device_node_name).clone();

                        let Some(source_node) = source_node else {
                            conn.send_response(DaemonResponse::Error(
                                "No device selected".to_string(),
                            ))?;
                            continue;
                        };

                        // The filtered side captures the virtual mic node,
                        // so the mic must be up and healthy. Without this
                        // guard a recording with the mic disabled would
                        // silently produce an empty/truncated filtered file
                        // (or hit the recorder's dont-fallback error).
                        // The target name comes from the live mic itself:
                        // state.mic_name can already hold a newer name from
                        // an Enable that has not recreated the mic yet, and
                        // recording against that name would miss the node.
                        {
                            let mic_guard = lock_or_recover(&virtual_mic);
                            if mic_guard.as_ref().is_some_and(VirtualMic::is_detached) {
                                drop(mic_guard);
                                conn.send_response(DaemonResponse::Error(
                                    "The input device is disconnected — the voice test would record silence"
                                        .to_string(),
                                ))?;
                                continue;
                            }
                            let live_mic_name = mic_guard
                                .as_ref()
                                .filter(|mic| mic.failure().is_none())
                                .map(|mic| mic.node_name().to_string());
                            if let Some(mic_name) = live_mic_name {
                                // Kill any in-flight recorder first: a second
                                // StartRecording (second IPC client, or GUI
                                // restarted mid-recording) would otherwise put
                                // two live writers on the same paths and
                                // truncate under the old writer.
                                if let Some(mut prev) = lock_or_recover(&recorder).take() {
                                    prev.stop();
                                }

                                match (get_original_recording_path(), get_filtered_recording_path())
                                {
                                    (Ok(original_path), Ok(filtered_path)) => {
                                        match NativeRecorder::start(
                                            &source_node,
                                            &mic_name,
                                            original_path,
                                            filtered_path,
                                        ) {
                                            Ok(r) => {
                                                *lock_or_recover(&recorder) = Some(r);
                                                // A voice test that starts
                                                // retires the record of one
                                                // that failed, the same way
                                                // enable and disable do.
                                                // Otherwise the reason a
                                                // recording died outlives
                                                // every later success and
                                                // only an enable clears it.
                                                state.set_last_error(None);
                                                DaemonResponse::Ok
                                            }
                                            Err(e) => {
                                                eprintln!("[recorder] NativeRecorder::start: {e}");
                                                DaemonResponse::Error(format!(
                                                    "Failed to start recording: {e}"
                                                ))
                                            }
                                        }
                                    }
                                    (Err(e), _) | (_, Err(e)) => {
                                        eprintln!("[recorder] recordings directory: {e}");
                                        DaemonResponse::Error(format!("Cannot record: {e}"))
                                    }
                                }
                            } else {
                                DaemonResponse::Error(
                                    "Cannot record filtered side: virtual mic is not active"
                                        .to_string(),
                                )
                            }
                        }
                    }
                    DaemonCommand::StopRecording => {
                        // The recorder finalizes its writers only after its
                        // loop exits, so a failure raised there — the header
                        // rewrite on a full disk, which is where hound's
                        // buffered tail usually lands — exists nowhere until
                        // stop() has joined. Taking the recorder out of the
                        // slot also puts it beyond the GetState reaper, so
                        // this is the last chance anything has to read it.
                        // Same ordering discipline as that reaper: record
                        // after dropping the recorder guard (#7 is held, #6
                        // must not be).
                        let stop_error = lock_or_recover(&recorder).take().and_then(|mut r| {
                            r.stop();
                            r.failure()
                        });
                        if let Some(msg) = stop_error {
                            state.set_last_error(Some(format!("Recording failed: {msg}")));
                        }
                        DaemonResponse::Ok
                    }
                    DaemonCommand::PlayOriginal => {
                        stop_player_of_kind(&player, PlaybackKind::Original);
                        stop_player_of_kind(&player, PlaybackKind::Filtered);

                        let original_path = match get_original_recording_path() {
                            Ok(path) => path,
                            Err(e) => {
                                eprintln!("[player] recordings directory: {e}");
                                conn.send_response(DaemonResponse::Error(format!(
                                    "Cannot play: {e}"
                                )))?;
                                continue;
                            }
                        };
                        if !original_path.exists() {
                            DaemonResponse::Error("No recording found".to_string())
                        } else {
                            match NativePlayer::start(&original_path) {
                                Ok(p) => {
                                    *lock_or_recover(&player) = Some((PlaybackKind::Original, p));
                                    state.set_last_error(None);
                                    DaemonResponse::Ok
                                }
                                Err(e) => {
                                    eprintln!("[player] NativePlayer::start: {e}");
                                    DaemonResponse::Error(format!("Failed to play: {e}"))
                                }
                            }
                        }
                    }
                    DaemonCommand::StopOriginal => {
                        stop_player_of_kind(&player, PlaybackKind::Original);
                        DaemonResponse::Ok
                    }
                    DaemonCommand::PlayFiltered => {
                        stop_player_of_kind(&player, PlaybackKind::Original);
                        stop_player_of_kind(&player, PlaybackKind::Filtered);

                        let filtered_path = match get_filtered_recording_path() {
                            Ok(path) => path,
                            Err(e) => {
                                eprintln!("[player] recordings directory: {e}");
                                conn.send_response(DaemonResponse::Error(format!(
                                    "Cannot play: {e}"
                                )))?;
                                continue;
                            }
                        };
                        if !filtered_path.exists() {
                            DaemonResponse::Error("No recording found".to_string())
                        } else {
                            match NativePlayer::start(&filtered_path) {
                                Ok(p) => {
                                    *lock_or_recover(&player) = Some((PlaybackKind::Filtered, p));
                                    state.set_last_error(None);
                                    DaemonResponse::Ok
                                }
                                Err(e) => {
                                    eprintln!("[player] NativePlayer::start: {e}");
                                    DaemonResponse::Error(format!("Failed to play: {e}"))
                                }
                            }
                        }
                    }
                    DaemonCommand::StopFiltered => {
                        stop_player_of_kind(&player, PlaybackKind::Filtered);
                        DaemonResponse::Ok
                    }
                    DaemonCommand::Shutdown => {
                        let mut mic_guard = lock_or_recover(&virtual_mic);
                        if let Some(mic) = mic_guard.take() {
                            mic.stop();
                        }
                        drop(mic_guard);

                        // Tear down recorder and player before exit(0) so
                        // WAVs are finalized (data-size header written) and
                        // audio threads join cleanly. Without this,
                        // exit(0) skips Drop → hound's reader sees a
                        // zero-length data chunk → silent empty playback.
                        if let Some(mut r) = lock_or_recover(&recorder).take() {
                            r.stop();
                        }
                        if let Some((_, mut p)) = lock_or_recover(&player).take() {
                            p.stop();
                        }

                        conn.send_response(DaemonResponse::Ok)?;
                        std::process::exit(0);
                    }
                };

                conn.send_response(response)?;
            }
            Err(e) => {
                if e.kind() == std::io::ErrorKind::UnexpectedEof {
                    break;
                }
                return Err(e);
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_supplied_node_name_outranks_a_supplied_id() {
        let plan = plan_device_lookup(
            Some("alsa_input.usb-Yeti".to_string()),
            Some(44),
            Some("alsa_input.builtin".to_string()),
        );
        assert_eq!(
            plan,
            DeviceLookup::ByName("alsa_input.usb-Yeti".to_string())
        );
    }

    #[test]
    fn an_id_is_used_when_no_name_is_supplied() {
        let plan = plan_device_lookup(None, Some(44), Some("alsa_input.builtin".to_string()));
        assert_eq!(plan, DeviceLookup::ById(44));
    }

    #[test]
    fn the_remembered_name_is_the_last_resort() {
        let plan = plan_device_lookup(None, None, Some("alsa_input.builtin".to_string()));
        assert_eq!(plan, DeviceLookup::ByName("alsa_input.builtin".to_string()));
    }

    #[test]
    fn nothing_supplied_and_nothing_remembered_selects_no_device() {
        assert_eq!(plan_device_lookup(None, None, None), DeviceLookup::NoDevice);
    }
}
