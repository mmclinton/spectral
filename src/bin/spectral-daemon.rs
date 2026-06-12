use spectral_lib::audio_devices::get_device_channels;
use spectral_lib::audio_processor::VirtualMic;
use spectral_lib::ipc::{DaemonCommand, DaemonResponse, DaemonServer, DaemonState};
use spectral_lib::persistence::{
    PersistedState, get_filtered_recording_path, get_original_recording_path, load_state,
    save_state,
};
use spectral_lib::state::{AppState, Status, is_valid_mic_name};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::Duration;

/// Helper to recover from poisoned mutexes.
/// In a daemon context, we prefer to continue operation with potentially stale data
/// rather than crash entirely, since the data is typically not critically inconsistent.
fn lock_or_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn path_to_str(path: &Path) -> Result<String, std::io::Error> {
    path.to_str().map(|s| s.to_owned()).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("Path contains non-UTF8 characters: {:?}", path),
        )
    })
}

fn main() {
    let state = Arc::new(AppState::new());
    let virtual_mic: Arc<Mutex<Option<VirtualMic>>> = Arc::new(Mutex::new(None));

    let recording_original: Arc<Mutex<Option<Child>>> = Arc::new(Mutex::new(None));
    let recording_filtered: Arc<Mutex<Option<Child>>> = Arc::new(Mutex::new(None));

    let playback_original: Arc<Mutex<Option<Child>>> = Arc::new(Mutex::new(None));
    let playback_filtered: Arc<Mutex<Option<Child>>> = Arc::new(Mutex::new(None));

    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();

    // Use a pipe to signal the signal handler thread to exit cleanly
    let (signal_tx, signal_rx) = std::sync::mpsc::channel::<()>();

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
        drop(signal_rx);
    });

    let mut persisted = load_state().unwrap_or_default();
    if !is_valid_mic_name(&persisted.mic_name) {
        eprintln!(
            "Persisted mic_name {:?} is invalid, resetting to default",
            persisted.mic_name
        );
        persisted.mic_name = "Spectral-Mic".to_string();
    }
    {
        *lock_or_recover(&state.threshold_val) = persisted.threshold;
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

    if let Some(mic) = lock_or_recover(&virtual_mic).take() {
        mic.stop();
    }

    for handle in client_threads {
        let _ = handle.join();
    }

    drop(signal_tx);
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
    // To prevent deadlocks, always acquire locks in this order:
    // 1. state.threshold_val
    // 2. state.mic_name
    // 3. state.selected_device_id
    // 4. state.status
    // 5. state.is_recording
    // 6. state.has_recording
    // 7. virtual_mic
    // 8. recording_original
    // 9. recording_filtered
    // 10. playback_original
    // 11. playback_filtered
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
                        *state
                            .threshold_val
                            .lock()
                            .expect("threshold_val mutex poisoned") = threshold;
                        *state.mic_name.lock().expect("mic_name mutex poisoned") = mic_name.clone();
                        *state
                            .selected_device_id
                            .lock()
                            .expect("selected_device_id mutex poisoned") = Some(device_id);

                        if let Err(e) = save_state(&PersistedState {
                            device_id: Some(device_id),
                            mic_name: mic_name.clone(),
                            threshold,
                        }) {
                            eprintln!("Failed to persist settings: {}", e);
                        }

                        let mut mic_guard = lock_or_recover(&virtual_mic);
                        if let Some(mic) = mic_guard.take() {
                            mic.stop();
                        }

                        // TODO: Validate channel detection for dynamically connected devices
                        let channels = get_device_channels(device_id);
                        match VirtualMic::create(state.clone(), device_id, &mic_name, channels) {
                            Ok(mic) => {
                                *mic_guard = Some(mic);
                                state.update_status(Status::Active);
                                DaemonResponse::Ok
                            }
                            Err(e) => {
                                state.update_status(Status::Inactive);
                                DaemonResponse::Error(format!(
                                    "Failed to create virtual mic: {}",
                                    e
                                ))
                            }
                        }
                    }
                    DaemonCommand::Disable => {
                        let mut mic_guard = lock_or_recover(&virtual_mic);
                        if let Some(mic) = mic_guard.take() {
                            mic.stop();
                        }
                        state.update_status(Status::Inactive);
                        DaemonResponse::Ok
                    }
                    DaemonCommand::GetState => {
                        let status = *state.status.lock().expect("status mutex poisoned");
                        let is_recording = *state
                            .is_recording
                            .lock()
                            .expect("is_recording mutex poisoned");
                        let has_recording = *state
                            .has_recording
                            .lock()
                            .expect("has_recording mutex poisoned");

                        let is_playing_original = {
                            let mut guard = playback_original
                                .lock()
                                .expect("playback_original mutex poisoned");
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
                            let mut guard = playback_filtered
                                .lock()
                                .expect("playback_filtered mutex poisoned");
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

                        let daemon_state = DaemonState {
                            status,
                            device_id: *state
                                .selected_device_id
                                .lock()
                                .expect("selected_device_id mutex poisoned"),
                            mic_name: state
                                .mic_name
                                .lock()
                                .expect("mic_name mutex poisoned")
                                .clone(),
                            threshold: *state
                                .threshold_val
                                .lock()
                                .expect("threshold_val mutex poisoned"),
                            is_recording,
                            has_recording,
                            is_playing_original,
                            is_playing_filtered,
                        };
                        DaemonResponse::State(daemon_state)
                    }
                    DaemonCommand::StartRecording => {
                        let device_id = *state
                            .selected_device_id
                            .lock()
                            .expect("selected_device_id mutex poisoned");
                        let mic_name = state
                            .mic_name
                            .lock()
                            .expect("mic_name mutex poisoned")
                            .clone();
                        let sink_name = format!("{}_sink", mic_name.replace(" ", "_"));

                        if device_id.is_none() {
                            return Ok(());
                        }

                        let source_node = match get_source_name(device_id.unwrap()) {
                            Ok(name) => name,
                            Err(e) => {
                                eprintln!("[Recording] Failed to get source name: {}", e);
                                *state
                                    .is_recording
                                    .lock()
                                    .expect("is_recording mutex poisoned") = false;
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

                        let monitor_source = format!("{}.monitor", sink_name);
                        let filtered_path_str = path_to_str(&filtered_path)?;
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
                                *recording_original
                                    .lock()
                                    .expect("recording_original mutex poisoned") = Some(orig);
                                *recording_filtered
                                    .lock()
                                    .expect("recording_filtered mutex poisoned") = Some(filt);
                                *state
                                    .is_recording
                                    .lock()
                                    .expect("is_recording mutex poisoned") = true;
                                *state
                                    .has_recording
                                    .lock()
                                    .expect("has_recording mutex poisoned") = false;
                                DaemonResponse::Ok
                            }
                            (Err(e), _) => {
                                eprintln!("[Recording] Failed to start original pw-record: {}", e);
                                if let Some(mut p) = lock_or_recover(&recording_original).take() {
                                    let _ = p.kill();
                                }
                                if let Some(mut p) = lock_or_recover(&recording_filtered).take() {
                                    let _ = p.kill();
                                }
                                DaemonResponse::Error(format!("Failed to start recording: {}", e))
                            }
                            (_, Err(e)) => {
                                eprintln!("[Recording] Failed to start filtered pw-record: {}", e);
                                if let Some(mut p) = lock_or_recover(&recording_original).take() {
                                    let _ = p.kill();
                                }
                                if let Some(mut p) = lock_or_recover(&recording_filtered).take() {
                                    let _ = p.kill();
                                }
                                DaemonResponse::Error(format!("Failed to start recording: {}", e))
                            }
                        }
                    }
                    DaemonCommand::StopRecording => {
                        if let Some(mut proc) = recording_original
                            .lock()
                            .expect("recording_original mutex poisoned")
                            .take()
                        {
                            let _ = proc.kill();
                            let _ = proc.wait();
                        }
                        if let Some(mut proc) = recording_filtered
                            .lock()
                            .expect("recording_filtered mutex poisoned")
                            .take()
                        {
                            let _ = proc.kill();
                            let _ = proc.wait();
                        }

                        *state
                            .is_recording
                            .lock()
                            .expect("is_recording mutex poisoned") = false;
                        *state
                            .has_recording
                            .lock()
                            .expect("has_recording mutex poisoned") = true;
                        DaemonResponse::Ok
                    }
                    DaemonCommand::PlayOriginal => {
                        if let Some(mut proc) = playback_original
                            .lock()
                            .expect("playback_original mutex poisoned")
                            .take()
                        {
                            let _ = proc.kill();
                            let _ = proc.wait();
                        }
                        if let Some(mut proc) = playback_filtered
                            .lock()
                            .expect("playback_filtered mutex poisoned")
                            .take()
                        {
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
                                    *playback_original
                                        .lock()
                                        .expect("playback_original mutex poisoned") = Some(proc);
                                    DaemonResponse::Ok
                                }
                                Err(e) => DaemonResponse::Error(format!("Failed to play: {}", e)),
                            }
                        }
                    }
                    DaemonCommand::StopOriginal => {
                        if let Some(mut proc) = playback_original
                            .lock()
                            .expect("playback_original mutex poisoned")
                            .take()
                        {
                            let _ = proc.kill();
                            let _ = proc.wait();
                        }
                        DaemonResponse::Ok
                    }
                    DaemonCommand::PlayFiltered => {
                        // Stop any existing playback first
                        if let Some(mut proc) = playback_original
                            .lock()
                            .expect("playback_original mutex poisoned")
                            .take()
                        {
                            let _ = proc.kill();
                            let _ = proc.wait();
                        }
                        if let Some(mut proc) = playback_filtered
                            .lock()
                            .expect("playback_filtered mutex poisoned")
                            .take()
                        {
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
                                    *playback_filtered
                                        .lock()
                                        .expect("playback_filtered mutex poisoned") = Some(proc);
                                    DaemonResponse::Ok
                                }
                                Err(e) => DaemonResponse::Error(format!("Failed to play: {}", e)),
                            }
                        }
                    }
                    DaemonCommand::StopFiltered => {
                        if let Some(mut proc) = playback_filtered
                            .lock()
                            .expect("playback_filtered mutex poisoned")
                            .take()
                        {
                            let _ = proc.kill();
                            let _ = proc.wait();
                        }
                        DaemonResponse::Ok
                    }
                    DaemonCommand::Shutdown => {
                        let mut mic_guard = lock_or_recover(&virtual_mic);
                        if let Some(mic) = mic_guard.take() {
                            mic.stop();
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

fn get_source_name(node_id: u32) -> Result<String, Box<dyn std::error::Error>> {
    let output = Command::new("wpctl")
        .args(["inspect", &node_id.to_string()])
        .output()?;

    if !output.status.success() {
        return Err(format!("Failed to inspect node {}", node_id).into());
    }

    let output_str = String::from_utf8_lossy(&output.stdout);

    for line in output_str.lines() {
        if line.contains("node.name")
            && let Some(name_part) = line.split('"').nth(1)
        {
            return Ok(name_part.to_string());
        }
    }

    Err(format!("Could not find node.name for node {}", node_id).into())
}
