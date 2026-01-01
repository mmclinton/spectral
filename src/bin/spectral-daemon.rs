use spectral_lib::audio_devices::get_device_channels;
use spectral_lib::audio_processor::VirtualMic;
use spectral_lib::ipc::{DaemonCommand, DaemonResponse, DaemonServer, DaemonState};
use spectral_lib::persistence::{
    PersistedState, get_filtered_recording_path, get_original_recording_path, load_state,
    save_state,
};
use spectral_lib::state::{AppState, Status};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

fn main() {
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

    let signal_thread = thread::spawn(move || {
        if signals.forever().next().is_some() {
            r.store(false, Ordering::SeqCst);
        }
    });

    let persisted = load_state().unwrap_or_default();
    {
        *state
            .threshold_val
            .lock()
            .expect("threshold_val mutex poisoned on startup") = persisted.threshold;
        *state
            .mic_name
            .lock()
            .expect("mic_name mutex poisoned on startup") = persisted.mic_name.clone();
        *state
            .selected_device_id
            .lock()
            .expect("selected_device_id mutex poisoned on startup") = persisted.device_id;
    }

    if persisted.is_active
        && let Some(device_id) = persisted.device_id
    {
        // TODO: Validate that get_device_channels returns accurate info for restored sessions
        // (device may have been unplugged/changed since last run)
        let channels = get_device_channels(device_id);
        match VirtualMic::create(state.clone(), device_id, &persisted.mic_name, channels) {
            Ok(mic) => {
                *virtual_mic
                    .lock()
                    .expect("virtual_mic mutex poisoned on startup") = Some(mic);
                state.update_status(Status::Active);
            }
            Err(e) => {
                eprintln!("Failed to restore virtual mic: {}", e);
            }
        }
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

        // Non-blocking accept with polling
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
                    ) {
                        if e.kind() != std::io::ErrorKind::WouldBlock
                            && e.kind() != std::io::ErrorKind::TimedOut
                        {
                            eprintln!("Client handler error: {}", e);
                        }
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

    if let Some(mic) = virtual_mic
        .lock()
        .expect("virtual_mic mutex poisoned during shutdown")
        .take()
    {
        mic.stop();
    }

    let persisted = PersistedState {
        is_active: false,
        device_id: *state
            .selected_device_id
            .lock()
            .expect("selected_device_id mutex poisoned during shutdown"),
        mic_name: state
            .mic_name
            .lock()
            .expect("mic_name mutex poisoned during shutdown")
            .clone(),
        threshold: *state
            .threshold_val
            .lock()
            .expect("threshold_val mutex poisoned during shutdown"),
    };
    let _ = save_state(&persisted);

    for handle in client_threads {
        let _ = handle.join();
    }

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
    // 1. state.pending_threshold
    // 2. state.pending_device_id
    // 3. state.pending_mic_name
    // 4. state.threshold_val
    // 5. state.mic_name
    // 6. state.selected_device_id
    // 7. state.status
    // 8. state.is_recording
    // 9. state.has_recording
    // 10. virtual_mic
    // 11. recording_original
    // 12. recording_filtered
    // 13. playback_original
    // 14. playback_filtered
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
                        let final_threshold = state
                            .pending_threshold
                            .lock()
                            .expect("pending_threshold mutex poisoned")
                            .unwrap_or(threshold);
                        let final_device_id = state
                            .pending_device_id
                            .lock()
                            .expect("pending_device_id mutex poisoned")
                            .unwrap_or(device_id);
                        let final_mic_name = state
                            .pending_mic_name
                            .lock()
                            .expect("pending_mic_name mutex poisoned")
                            .clone()
                            .unwrap_or(mic_name.clone());

                        *state
                            .threshold_val
                            .lock()
                            .expect("threshold_val mutex poisoned") = final_threshold;
                        *state.mic_name.lock().expect("mic_name mutex poisoned") =
                            final_mic_name.clone();
                        *state
                            .selected_device_id
                            .lock()
                            .expect("selected_device_id mutex poisoned") = Some(final_device_id);

                        *state
                            .pending_threshold
                            .lock()
                            .expect("pending_threshold mutex poisoned") = None;
                        *state
                            .pending_device_id
                            .lock()
                            .expect("pending_device_id mutex poisoned") = None;
                        *state
                            .pending_mic_name
                            .lock()
                            .expect("pending_mic_name mutex poisoned") = None;

                        let mut mic_guard = virtual_mic.lock().expect("virtual_mic mutex poisoned");
                        if let Some(mic) = mic_guard.take() {
                            mic.stop();
                        }

                        // TODO: Validate channel detection for dynamically connected devices
                        let channels = get_device_channels(final_device_id);
                        match VirtualMic::create(
                            state.clone(),
                            final_device_id,
                            &final_mic_name,
                            channels,
                        ) {
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
                        let mut mic_guard = virtual_mic.lock().expect("virtual_mic mutex poisoned");
                        if let Some(mic) = mic_guard.take() {
                            mic.stop();
                        }
                        state.update_status(Status::Inactive);
                        DaemonResponse::Ok
                    }
                    DaemonCommand::UpdateThreshold(threshold) => {
                        let applied_threshold = *state
                            .threshold_val
                            .lock()
                            .expect("threshold_val mutex poisoned");

                        if threshold == applied_threshold {
                            *state
                                .pending_threshold
                                .lock()
                                .expect("pending_threshold mutex poisoned") = None;

                            let has_pending_device = state
                                .pending_device_id
                                .lock()
                                .expect("pending_device_id mutex poisoned")
                                .is_some();
                            let has_pending_name = state
                                .pending_mic_name
                                .lock()
                                .expect("pending_mic_name mutex poisoned")
                                .is_some();

                            if !has_pending_device && !has_pending_name {
                                let status = *state.status.lock().expect("status mutex poisoned");
                                if let Status::Pending = status {
                                    state.update_status(Status::Active);
                                }
                            }
                        } else {
                            *state
                                .pending_threshold
                                .lock()
                                .expect("pending_threshold mutex poisoned") = Some(threshold);

                            let status = *state.status.lock().expect("status mutex poisoned");
                            if let Status::Active = status {
                                state.update_status(Status::Pending);
                            }
                        }
                        DaemonResponse::Ok
                    }
                    DaemonCommand::UpdateMicName(mic_name) => {
                        let applied_name = state
                            .mic_name
                            .lock()
                            .expect("mic_name mutex poisoned")
                            .clone();

                        if mic_name == applied_name {
                            *state
                                .pending_mic_name
                                .lock()
                                .expect("pending_mic_name mutex poisoned") = None;

                            let has_pending_threshold = state
                                .pending_threshold
                                .lock()
                                .expect("pending_threshold mutex poisoned")
                                .is_some();
                            let has_pending_device = state
                                .pending_device_id
                                .lock()
                                .expect("pending_device_id mutex poisoned")
                                .is_some();

                            if !has_pending_threshold && !has_pending_device {
                                let status = *state.status.lock().expect("status mutex poisoned");
                                if let Status::Pending = status {
                                    state.update_status(Status::Active);
                                }
                            }
                        } else {
                            *state
                                .pending_mic_name
                                .lock()
                                .expect("pending_mic_name mutex poisoned") = Some(mic_name);

                            let status = *state.status.lock().expect("status mutex poisoned");
                            if let Status::Active = status {
                                state.update_status(Status::Pending);
                            }
                        }
                        DaemonResponse::Ok
                    }
                    DaemonCommand::UpdateDevice(device_id) => {
                        let applied_device = *state
                            .selected_device_id
                            .lock()
                            .expect("selected_device_id mutex poisoned");

                        if Some(device_id) == applied_device {
                            *state
                                .pending_device_id
                                .lock()
                                .expect("pending_device_id mutex poisoned") = None;

                            let has_pending_threshold = state
                                .pending_threshold
                                .lock()
                                .expect("pending_threshold mutex poisoned")
                                .is_some();
                            let has_pending_name = state
                                .pending_mic_name
                                .lock()
                                .expect("pending_mic_name mutex poisoned")
                                .is_some();

                            if !has_pending_threshold && !has_pending_name {
                                let status = *state.status.lock().expect("status mutex poisoned");
                                if let Status::Pending = status {
                                    state.update_status(Status::Active);
                                }
                            }
                        } else {
                            *state
                                .pending_device_id
                                .lock()
                                .expect("pending_device_id mutex poisoned") = Some(device_id);

                            let status = *state.status.lock().expect("status mutex poisoned");
                            if let Status::Active = status {
                                state.update_status(Status::Pending);
                            }
                        }
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
                                    Ok(None) => true, // Still running
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

                        eprintln!(
                            "[Recording] device_id: {:?}, mic_name: {}, sink_name: {}",
                            device_id, mic_name, sink_name
                        );

                        if device_id.is_none() {
                            eprintln!("[Recording] ERROR: No device selected");
                            return Ok(());
                        }

                        let source_node = match get_source_name(device_id.unwrap()) {
                            Ok(name) => {
                                eprintln!("[Recording] source_node: {}", name);
                                name
                            }
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

                        eprintln!("[Recording] original_path: {:?}", original_path);
                        eprintln!("[Recording] filtered_path: {:?}", filtered_path);

                        // Start recording original input (from the physical mic)
                        // pw-record uses --target for the source node name
                        eprintln!(
                            "[Recording] Starting pw-record for original with target: {}",
                            source_node
                        );
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
                                original_path.to_str().unwrap(),
                            ])
                            .stdin(Stdio::null())
                            .stdout(Stdio::inherit())
                            .stderr(Stdio::inherit())
                            .spawn();

                        let monitor_source = format!("{}.monitor", sink_name);
                        eprintln!(
                            "[Recording] Starting pw-record for filtered with target: {}",
                            monitor_source
                        );
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
                                filtered_path.to_str().unwrap(),
                            ])
                            .stdin(Stdio::null())
                            .stdout(Stdio::inherit())
                            .stderr(Stdio::inherit())
                            .spawn();

                        match (original_proc, filtered_proc) {
                            (Ok(orig), Ok(filt)) => {
                                eprintln!(
                                    "[Recording] Both pw-record processes started successfully"
                                );
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
                                if let Some(mut p) =
                                    recording_original.lock().expect("mutex").take()
                                {
                                    let _ = p.kill();
                                }
                                if let Some(mut p) =
                                    recording_filtered.lock().expect("mutex").take()
                                {
                                    let _ = p.kill();
                                }
                                DaemonResponse::Error(format!("Failed to start recording: {}", e))
                            }
                            (_, Err(e)) => {
                                eprintln!("[Recording] Failed to start filtered pw-record: {}", e);
                                if let Some(mut p) =
                                    recording_original.lock().expect("mutex").take()
                                {
                                    let _ = p.kill();
                                }
                                if let Some(mut p) =
                                    recording_filtered.lock().expect("mutex").take()
                                {
                                    let _ = p.kill();
                                }
                                DaemonResponse::Error(format!("Failed to start recording: {}", e))
                            }
                        }
                    }
                    DaemonCommand::StopRecording => {
                        eprintln!("[Recording] StopRecording command received");
                        if let Some(mut proc) = recording_original
                            .lock()
                            .expect("recording_original mutex poisoned")
                            .take()
                        {
                            eprintln!("[Recording] Killing original recording process");
                            let _ = proc.kill();
                            let exit_status = proc.wait();
                            eprintln!(
                                "[Recording] Original process exit status: {:?}",
                                exit_status
                            );
                        } else {
                            eprintln!("[Recording] No original recording process found");
                        }
                        if let Some(mut proc) = recording_filtered
                            .lock()
                            .expect("recording_filtered mutex poisoned")
                            .take()
                        {
                            eprintln!("[Recording] Killing filtered recording process");
                            let _ = proc.kill();
                            let exit_status = proc.wait();
                            eprintln!(
                                "[Recording] Filtered process exit status: {:?}",
                                exit_status
                            );
                        } else {
                            eprintln!("[Recording] No filtered recording process found");
                        }

                        let original_path = get_original_recording_path();
                        let filtered_path = get_filtered_recording_path();
                        eprintln!(
                            "[Recording] original_path exists: {}, size: {:?}",
                            original_path.exists(),
                            std::fs::metadata(&original_path).map(|m| m.len()).ok()
                        );
                        eprintln!(
                            "[Recording] filtered_path exists: {}, size: {:?}",
                            filtered_path.exists(),
                            std::fs::metadata(&filtered_path).map(|m| m.len()).ok()
                        );

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
                            match Command::new("pw-play")
                                .arg(original_path.to_str().unwrap())
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
                            match Command::new("pw-play")
                                .arg(filtered_path.to_str().unwrap())
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
                        let mut mic_guard = virtual_mic.lock().expect("virtual_mic mutex poisoned");
                        if let Some(mic) = mic_guard.take() {
                            mic.stop();
                        }

                        let persisted = PersistedState {
                            is_active: false,
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
                        };
                        let _ = save_state(&persisted);

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
        if line.contains("node.name") {
            if let Some(name_part) = line.split('"').nth(1) {
                return Ok(name_part.to_string());
            }
        }
    }

    Err(format!("Could not find node.name for node {}", node_id).into())
}
