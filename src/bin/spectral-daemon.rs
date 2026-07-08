use spectral_lib::audio_devices::{get_device_channels, get_node_name};
use spectral_lib::audio_processor::{VirtualMic, reap_leaked_modules};
use spectral_lib::ipc::{DaemonCommand, DaemonResponse, DaemonServer, DaemonState};
use spectral_lib::persistence::{
    PersistedState, get_filtered_recording_path, get_original_recording_path, load_state,
    remove_legacy_recordings, save_state,
};
use spectral_lib::state::{AppState, Status, is_valid_mic_name, lock_or_recover, monitor_name};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

fn path_to_str(path: &Path) -> Result<String, std::io::Error> {
    path.to_str().map(|s| s.to_owned()).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("Path contains non-UTF8 characters: {:?}", path),
        )
    })
}

fn main() {
    if std::env::args().skip(1).any(|arg| arg == "--reap") {
        match reap_leaked_modules() {
            Ok(n) => println!("[reap] unloaded {n} leaked module(s)"),
            Err(e) => {
                eprintln!("[reap] failed: {e}");
                std::process::exit(1);
            }
        }
        return;
    }

    let state = Arc::new(AppState::new());
    let virtual_mic: Arc<Mutex<Option<VirtualMic>>> = Arc::new(Mutex::new(None));

    let recording_original: Arc<Mutex<Option<Child>>> = Arc::new(Mutex::new(None));
    let recording_filtered: Arc<Mutex<Option<Child>>> = Arc::new(Mutex::new(None));

    let playback_original: Arc<Mutex<Option<Child>>> = Arc::new(Mutex::new(None));
    let playback_filtered: Arc<Mutex<Option<Child>>> = Arc::new(Mutex::new(None));

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
        *lock_or_recover(&state.mic_name) = persisted.mic_name.clone();
        *lock_or_recover(&state.selected_device_id) = persisted.device_id;
    }

    let server = match DaemonServer::bind() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to bind daemon socket: {}", e);
            return;
        }
    };

    // Now that we hold the single-instance lock, clear any modules an earlier
    // instance leaked (a crash or SIGKILL bypasses VirtualMic's Drop).
    match reap_leaked_modules() {
        Ok(0) => {}
        Ok(n) => eprintln!("[startup] reaped {n} leaked virtual-mic module(s)"),
        Err(e) => eprintln!("[startup] module reap skipped: {e}"),
    }

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
                let recording_original = recording_original.clone();
                let recording_filtered = recording_filtered.clone();
                let playback_original = playback_original.clone();
                let playback_filtered = playback_filtered.clone();

                let handle = thread::spawn(move || {
                    if let Err(e) = handle_client(
                        &mut conn,
                        state,
                        virtual_mic,
                        recording_original,
                        recording_filtered,
                        playback_original,
                        playback_filtered,
                    ) && e.kind() != std::io::ErrorKind::WouldBlock
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

    if let Some(mic) = lock_or_recover(&virtual_mic).take()
        && let Err(e) = mic.stop()
    {
        eprintln!("[shutdown] virtual mic teardown incomplete: {e}");
    }

    for handle in client_threads {
        let _ = handle.join();
    }

    signal_handle.close();

    let _ = signal_thread.join();
}

fn handle_client(
    conn: &mut spectral_lib::ipc::DaemonConnection,
    state: Arc<AppState>,
    virtual_mic: Arc<Mutex<Option<VirtualMic>>>,
    recording_original: Arc<Mutex<Option<Child>>>,
    recording_filtered: Arc<Mutex<Option<Child>>>,
    playback_original: Arc<Mutex<Option<Child>>>,
    playback_filtered: Arc<Mutex<Option<Child>>>,
) -> Result<(), std::io::Error> {
    // LOCK ORDERING CONVENTION:
    // To prevent deadlocks, always acquire locks in this order
    // (state.threshold_val is an atomic and exempt):
    // 1. state.mic_name
    // 2. state.selected_device_id
    // 3. virtual_mic
    // 4. state.status
    // 5. state.last_error
    // 6. recording_original
    // 7. recording_filtered
    // 8. playback_original
    // 9. playback_filtered
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
                    } => {
                        if !is_valid_mic_name(&mic_name) {
                            conn.send_response(DaemonResponse::Error(
                                "Invalid mic name: only [A-Za-z0-9_-] allowed, 1-21 chars"
                                    .to_string(),
                            ))?;
                            continue;
                        }
                        let threshold = threshold.clamp(0, 100);
                        state.threshold_val.store(threshold, Ordering::Relaxed);
                        *lock_or_recover(&state.mic_name) = mic_name.clone();
                        *lock_or_recover(&state.selected_device_id) = Some(device_id);

                        if let Err(e) = save_state(&PersistedState {
                            device_id: Some(device_id),
                            mic_name: mic_name.clone(),
                            threshold,
                        }) {
                            eprintln!("Failed to persist settings: {}", e);
                        }

                        let mut mic_guard = lock_or_recover(&virtual_mic);
                        if let Some(mic) = mic_guard.take()
                            && let Err(e) = mic.stop()
                        {
                            eprintln!("[enable] previous mic teardown incomplete: {e}");
                        }
                        // Sweep any orphaned modules before creating fresh ones so a
                        // failed prior teardown can't accumulate duplicates.
                        if let Err(e) = reap_leaked_modules() {
                            eprintln!("[enable] module reap skipped: {e}");
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
                    DaemonCommand::Disable => {
                        let mut mic_guard = lock_or_recover(&virtual_mic);
                        let result = mic_guard.take().map_or(Ok(()), VirtualMic::stop);
                        // The pipeline is stopped even when module unloading
                        // fails, so Inactive is the honest status either way.
                        state.update_status(Status::Inactive);
                        match result {
                            Ok(()) => {
                                state.set_last_error(None);
                                DaemonResponse::Ok
                            }
                            Err(e) => {
                                let msg = format!(
                                    "Disable incomplete, modules will be reaped on next enable: {e}"
                                );
                                state.set_last_error(Some(msg.clone()));
                                DaemonResponse::Error(msg)
                            }
                        }
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
                            if let Err(e) = mic.stop() {
                                eprintln!("[getstate] dead mic teardown incomplete: {e}");
                            }
                            state.update_status(Status::Inactive);
                            state.set_last_error(Some(format!("Virtual mic stopped: {msg}")));
                        }

                        let status = *lock_or_recover(&state.status);

                        let is_playing_original = {
                            let mut guard = lock_or_recover(&playback_original);
                            if let Some(ref mut proc) = *guard {
                                match proc.try_wait() {
                                    Ok(Some(_)) => {
                                        *guard = None;
                                        false
                                    }
                                    Ok(None) => true,
                                    Err(_) => {
                                        *guard = None;
                                        false
                                    }
                                }
                            } else {
                                false
                            }
                        };

                        let is_playing_filtered = {
                            let mut guard = lock_or_recover(&playback_filtered);
                            if let Some(ref mut proc) = *guard {
                                match proc.try_wait() {
                                    Ok(Some(_)) => {
                                        *guard = None;
                                        false
                                    }
                                    Ok(None) => true,
                                    Err(_) => {
                                        *guard = None;
                                        false
                                    }
                                }
                            } else {
                                false
                            }
                        };

                        let threshold = state.threshold_val.load(Ordering::Relaxed);
                        let mic_name = lock_or_recover(&state.mic_name).clone();
                        let device_id = *lock_or_recover(&state.selected_device_id);
                        let last_error = lock_or_recover(&state.last_error).clone();
                        let daemon_state = DaemonState {
                            status,
                            device_id,
                            mic_name,
                            threshold,
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

                        let original_path = get_original_recording_path();
                        let filtered_path = get_filtered_recording_path();
                        let original_path_str = path_to_str(&original_path)?;
                        let filtered_path_str = path_to_str(&filtered_path)?;

                        if let Some(mut proc) = lock_or_recover(&recording_original).take() {
                            let _ = proc.kill();
                            let _ = proc.wait();
                        }
                        if let Some(mut proc) = lock_or_recover(&recording_filtered).take() {
                            let _ = proc.kill();
                            let _ = proc.wait();
                        }

                        let original_proc = Command::new("pw-record")
                            .args([
                                "--target",
                                &source_node,
                                "--rate",
                                "48000",
                                "--channels",
                                "1",
                                "--format",
                                "f32",
                                &original_path_str,
                            ])
                            .stdin(Stdio::null())
                            .stdout(Stdio::inherit())
                            .stderr(Stdio::inherit())
                            .spawn();

                        let monitor_source = monitor_name(&mic_name);
                        let filtered_proc = Command::new("pw-record")
                            .args([
                                "--target",
                                &monitor_source,
                                "--rate",
                                "48000",
                                "--channels",
                                "1",
                                "--format",
                                "f32",
                                &filtered_path_str,
                            ])
                            .stdin(Stdio::null())
                            .stdout(Stdio::inherit())
                            .stderr(Stdio::inherit())
                            .spawn();

                        match (original_proc, filtered_proc) {
                            (Ok(orig), Ok(filt)) => {
                                *lock_or_recover(&recording_original) = Some(orig);
                                *lock_or_recover(&recording_filtered) = Some(filt);
                                DaemonResponse::Ok
                            }
                            (Err(e), filt) => {
                                eprintln!("[Recording] Failed to start original pw-record: {}", e);
                                if let Ok(mut p) = filt {
                                    let _ = p.kill();
                                    let _ = p.wait();
                                }
                                DaemonResponse::Error(format!("Failed to start recording: {}", e))
                            }
                            (Ok(mut orig), Err(e)) => {
                                eprintln!("[Recording] Failed to start filtered pw-record: {}", e);
                                let _ = orig.kill();
                                let _ = orig.wait();
                                DaemonResponse::Error(format!("Failed to start recording: {}", e))
                            }
                        }
                    }
                    DaemonCommand::StopRecording => {
                        if let Some(mut proc) = lock_or_recover(&recording_original).take() {
                            let _ = proc.kill();
                            let _ = proc.wait();
                        }
                        if let Some(mut proc) = lock_or_recover(&recording_filtered).take() {
                            let _ = proc.kill();
                            let _ = proc.wait();
                        }

                        DaemonResponse::Ok
                    }
                    DaemonCommand::PlayOriginal => {
                        if let Some(mut proc) = lock_or_recover(&playback_original).take() {
                            let _ = proc.kill();
                            let _ = proc.wait();
                        }
                        if let Some(mut proc) = lock_or_recover(&playback_filtered).take() {
                            let _ = proc.kill();
                            let _ = proc.wait();
                        }

                        let original_path = get_original_recording_path();
                        if !original_path.exists() {
                            DaemonResponse::Error("No recording found".to_string())
                        } else {
                            let original_path_str = match path_to_str(&original_path) {
                                Ok(s) => s,
                                Err(e) => {
                                    conn.send_response(DaemonResponse::Error(format!(
                                        "Invalid path: {}",
                                        e
                                    )))?;
                                    continue;
                                }
                            };
                            match Command::new("pw-play")
                                .arg(&original_path_str)
                                .stdin(Stdio::null())
                                .stdout(Stdio::null())
                                .stderr(Stdio::null())
                                .spawn()
                            {
                                Ok(proc) => {
                                    *lock_or_recover(&playback_original) = Some(proc);
                                    DaemonResponse::Ok
                                }
                                Err(e) => DaemonResponse::Error(format!("Failed to play: {}", e)),
                            }
                        }
                    }
                    DaemonCommand::StopOriginal => {
                        if let Some(mut proc) = lock_or_recover(&playback_original).take() {
                            let _ = proc.kill();
                            let _ = proc.wait();
                        }
                        DaemonResponse::Ok
                    }
                    DaemonCommand::PlayFiltered => {
                        // Stop any existing playback first
                        if let Some(mut proc) = lock_or_recover(&playback_original).take() {
                            let _ = proc.kill();
                            let _ = proc.wait();
                        }
                        if let Some(mut proc) = lock_or_recover(&playback_filtered).take() {
                            let _ = proc.kill();
                            let _ = proc.wait();
                        }

                        let filtered_path = get_filtered_recording_path();
                        if !filtered_path.exists() {
                            DaemonResponse::Error("No recording found".to_string())
                        } else {
                            let filtered_path_str = match path_to_str(&filtered_path) {
                                Ok(s) => s,
                                Err(e) => {
                                    conn.send_response(DaemonResponse::Error(format!(
                                        "Invalid path: {}",
                                        e
                                    )))?;
                                    continue;
                                }
                            };
                            match Command::new("pw-play")
                                .arg(&filtered_path_str)
                                .stdin(Stdio::null())
                                .stdout(Stdio::null())
                                .stderr(Stdio::null())
                                .spawn()
                            {
                                Ok(proc) => {
                                    *lock_or_recover(&playback_filtered) = Some(proc);
                                    DaemonResponse::Ok
                                }
                                Err(e) => DaemonResponse::Error(format!("Failed to play: {}", e)),
                            }
                        }
                    }
                    DaemonCommand::StopFiltered => {
                        if let Some(mut proc) = lock_or_recover(&playback_filtered).take() {
                            let _ = proc.kill();
                            let _ = proc.wait();
                        }
                        DaemonResponse::Ok
                    }
                    DaemonCommand::Shutdown => {
                        let mut mic_guard = lock_or_recover(&virtual_mic);
                        if let Some(mic) = mic_guard.take()
                            && let Err(e) = mic.stop()
                        {
                            eprintln!("[shutdown] virtual mic teardown incomplete: {e}");
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
