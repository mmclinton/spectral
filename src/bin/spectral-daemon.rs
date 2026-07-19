//! The long-lived backend: owns the virtual mic and all audio state,
//! persists settings, and serves GUI/CLI clients on the Unix socket with
//! one handler thread per connection. SIGTERM/SIGINT tear everything down.

use spectral_lib::audio_devices::{find_node_id, get_device_channels, get_node_name};
use spectral_lib::audio_processor::VirtualMic;
use spectral_lib::ipc::{
    DaemonCommand, DaemonResponse, DaemonServer, DaemonState, PROTOCOL_VERSION,
};
use spectral_lib::persistence::{
    PersistedState, get_filtered_recording_path, get_original_recording_path, load_state,
    remove_legacy_recordings, save_state,
};
use spectral_lib::state::{AppState, MIC_NAME_MAX_LEN, Status, is_valid_mic_name, lock_or_recover};
use spectral_lib::{NativePlayer, NativeRecorder, PlaybackKind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

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
        }
    });

    let mut persisted = load_state().unwrap_or_default();
    if !is_valid_mic_name(&persisted.mic_name) {
        eprintln!(
            "Persisted mic_name {:?} is invalid, resetting to default",
            persisted.mic_name
        );
        persisted.mic_name = "Spectral-Mic".to_string();
    }
    if !(0..=100).contains(&persisted.threshold) {
        eprintln!(
            "Persisted threshold {} is out of range, clamping",
            persisted.threshold
        );
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

    let server = match DaemonServer::bind() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to bind daemon socket: {}", e);
            return;
        }
    };

    remove_legacy_recordings();

    let mut client_threads: Vec<thread::JoinHandle<()>> = Vec::new();

    loop {
        if !running.load(Ordering::SeqCst) {
            break;
        }

        client_threads.retain(|handle| !handle.is_finished());

        match server.accept() {
            Ok(mut conn) => {
                let state = state.clone();
                let virtual_mic = virtual_mic.clone();
                let recorder = recorder.clone();
                let player = player.clone();

                let handle = thread::spawn(move || {
                    if let Err(e) = handle_client(&mut conn, state, virtual_mic, recorder, player)
                        && e.kind() != std::io::ErrorKind::WouldBlock
                        && e.kind() != std::io::ErrorKind::TimedOut
                    {
                        eprintln!("Client handler error: {}", e);
                    }
                });

                client_threads.push(handle);
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(100));
            }
            Err(e) => {
                if e.kind() != std::io::ErrorKind::Interrupted {
                    eprintln!("Failed to accept connection: {}", e);
                }
                thread::sleep(Duration::from_millis(100));
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
    mic_name: Option<String>,
    threshold: Option<i32>,
    rumble_filter: Option<bool>,
    auto_level: Option<bool>,
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
        mic_name,
        threshold,
        rumble_filter,
        auto_level,
    } = request;
    let mic_name = match mic_name {
        Some(name) => {
            if !is_valid_mic_name(&name) {
                return DaemonResponse::Error(format!(
                    "Invalid mic name: only [A-Za-z0-9_-] allowed, 1-{MIC_NAME_MAX_LEN} chars"
                ));
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
    let (device_id, device_node_name) = match device_id {
        Some(id) => match get_node_name(id) {
            Ok(name) => (id, name),
            Err(e) => {
                return DaemonResponse::Error(format!(
                    "Cannot use device {id}: {e} (stale id? refresh the device list or run `spectral devices`)"
                ));
            }
        },
        None => {
            let remembered = lock_or_recover(&state.device_node_name).clone();
            match remembered {
                Some(name) => match find_node_id(&name) {
                    Ok(Some(id)) => (id, name),
                    Ok(None) => {
                        return DaemonResponse::Error(format!(
                            "Remembered device \"{name}\" is not present — run `spectral devices` and pass --device"
                        ));
                    }
                    Err(e) => {
                        return DaemonResponse::Error(format!("Failed to query PipeWire: {e}"));
                    }
                },
                None => {
                    return DaemonResponse::Error(
                        "No device selected — pass --device <id> (see `spectral devices`)"
                            .to_string(),
                    );
                }
            }
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
        device_node_name: Some(device_node_name),
        mic_name: mic_name.clone(),
        threshold,
        rumble_filter,
        auto_level,
    }) {
        eprintln!("Failed to persist settings: {}", e);
    }

    let mut mic_guard = lock_or_recover(virtual_mic);
    if let Some(mic) = mic_guard.take() {
        mic.stop();
    }

    // TODO: Validate channel detection for dynamically connected devices
    let channels = get_device_channels(device_id);
    match VirtualMic::create(state.clone(), device_id, &mic_name, channels) {
        Ok(mic) => {
            *mic_guard = Some(mic);
            state.update_status(Status::Active);
            state.set_last_error(None);
            DaemonResponse::Ok
        }
        Err(e) => {
            state.update_status(Status::Inactive);
            let msg = format!("Failed to create virtual mic: {}", e);
            state.set_last_error(Some(msg.clone()));
            DaemonResponse::Error(msg)
        }
    }
}

fn handle_client(
    conn: &mut spectral_lib::ipc::DaemonConnection,
    state: Arc<AppState>,
    virtual_mic: Arc<Mutex<Option<VirtualMic>>>,
    recorder: Arc<Mutex<Option<NativeRecorder>>>,
    player: Arc<Mutex<Option<(PlaybackKind, NativePlayer)>>>,
) -> Result<(), std::io::Error> {
    // LOCK ORDERING CONVENTION:
    // To prevent deadlocks, always acquire locks in this order
    // (state.threshold_val is an atomic and exempt):
    // 1. state.mic_name
    // 2. state.selected_device_id
    // 3. state.device_node_name
    // 4. virtual_mic
    // 5. state.status
    // 6. state.last_error
    // 7. recorder
    // 8. player
    //
    // Important: Release locks as soon as possible by limiting scope.
    // Never hold multiple locks longer than necessary.

    loop {
        match conn.read_command() {
            Ok(command) => {
                let response = match command {
                    DaemonCommand::Enable {
                        device_id,
                        mic_name,
                        threshold,
                        rumble_filter,
                        auto_level,
                    } => handle_enable(
                        &state,
                        &virtual_mic,
                        EnableRequest {
                            device_id,
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
                        let dead_mic = {
                            let mut mic_guard = lock_or_recover(&virtual_mic);
                            match mic_guard.as_ref().and_then(VirtualMic::failure) {
                                Some(msg) => mic_guard.take().map(|mic| (mic, msg)),
                                None => None,
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
                        // above.
                        let mut is_playing_original = false;
                        let mut is_playing_filtered = false;
                        {
                            let mut guard = lock_or_recover(&player);
                            if let Some((kind, p)) = guard.as_ref() {
                                if p.is_finished() {
                                    if let Some(msg) = p.failure() {
                                        state.set_last_error(Some(format!(
                                            "Playback stopped: {msg}"
                                        )));
                                    }
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
                        }

                        // Reap recorder failures into last_error for
                        // visibility. Recorders are not surfaced in
                        // DaemonState (no recording-active field exists
                        // today and adding one changes the wire format,
                        // out of scope), but a failed recording still
                        // deserves to be torn down and reported.
                        {
                            let mut guard = lock_or_recover(&recorder);
                            if let Some(r) = guard.as_ref()
                                && let Some(msg) = r.failure()
                            {
                                state.set_last_error(Some(format!("Recording stopped: {msg}")));
                                if let Some(mut finished) = guard.take() {
                                    finished.stop();
                                }
                            }
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
                            is_playing_original,
                            is_playing_filtered,
                            last_error,
                        };
                        DaemonResponse::State(daemon_state)
                    }
                    DaemonCommand::StartRecording => {
                        let device_id = *lock_or_recover(&state.selected_device_id);
                        let mic_name = lock_or_recover(&state.mic_name).clone();

                        let Some(device_id) = device_id else {
                            conn.send_response(DaemonResponse::Error(
                                "No device selected".to_string(),
                            ))?;
                            continue;
                        };

                        let source_node = match get_node_name(device_id) {
                            Ok(name) => name,
                            Err(e) => {
                                eprintln!("[Recording] Failed to get source name: {}", e);
                                conn.send_response(DaemonResponse::Error(format!(
                                    "Failed to get source: {}",
                                    e
                                )))?;
                                continue;
                            }
                        };

                        // The filtered side captures the virtual mic node,
                        // so the mic must be up and healthy. Without this
                        // guard a recording with the mic disabled would
                        // silently produce an empty/truncated filtered file
                        // (or hit the recorder's dont-reconnect error).
                        {
                            let mic_guard = lock_or_recover(&virtual_mic);
                            let mic_ok = mic_guard
                                .as_ref()
                                .is_some_and(|mic| mic.failure().is_none());
                            if !mic_ok {
                                DaemonResponse::Error(
                                    "Cannot record filtered side: virtual mic is not active"
                                        .to_string(),
                                )
                            } else {
                                // Kill any in-flight recorder first: a second
                                // StartRecording (second IPC client, or GUI
                                // restarted mid-recording) would otherwise put
                                // two live writers on the same paths and
                                // truncate under the old writer.
                                if let Some(mut prev) = lock_or_recover(&recorder).take() {
                                    prev.stop();
                                }

                                let original_path = get_original_recording_path();
                                let filtered_path = get_filtered_recording_path();
                                match NativeRecorder::start(
                                    &source_node,
                                    &mic_name,
                                    original_path,
                                    filtered_path,
                                ) {
                                    Ok(r) => {
                                        *lock_or_recover(&recorder) = Some(r);
                                        DaemonResponse::Ok
                                    }
                                    Err(e) => {
                                        eprintln!("[Recording] NativeRecorder::start: {}", e);
                                        DaemonResponse::Error(format!(
                                            "Failed to start recording: {}",
                                            e
                                        ))
                                    }
                                }
                            }
                        }
                    }
                    DaemonCommand::StopRecording => {
                        if let Some(mut r) = lock_or_recover(&recorder).take() {
                            r.stop();
                        }
                        DaemonResponse::Ok
                    }
                    DaemonCommand::PlayOriginal => {
                        stop_player_of_kind(&player, PlaybackKind::Original);
                        stop_player_of_kind(&player, PlaybackKind::Filtered);

                        let original_path = get_original_recording_path();
                        if !original_path.exists() {
                            DaemonResponse::Error("No recording found".to_string())
                        } else {
                            match NativePlayer::start(&original_path) {
                                Ok(p) => {
                                    *lock_or_recover(&player) = Some((PlaybackKind::Original, p));
                                    DaemonResponse::Ok
                                }
                                Err(e) => {
                                    eprintln!("[Playback] NativePlayer::start: {}", e);
                                    DaemonResponse::Error(format!("Failed to play: {}", e))
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

                        let filtered_path = get_filtered_recording_path();
                        if !filtered_path.exists() {
                            DaemonResponse::Error("No recording found".to_string())
                        } else {
                            match NativePlayer::start(&filtered_path) {
                                Ok(p) => {
                                    *lock_or_recover(&player) = Some((PlaybackKind::Filtered, p));
                                    DaemonResponse::Ok
                                }
                                Err(e) => {
                                    eprintln!("[Playback] NativePlayer::start: {}", e);
                                    DaemonResponse::Error(format!("Failed to play: {}", e))
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
