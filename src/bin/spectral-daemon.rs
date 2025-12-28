use spectral_lib::audio_processor::VirtualMic;
use spectral_lib::ipc::{DaemonCommand, DaemonResponse, DaemonServer, DaemonState};
use spectral_lib::persistence::{PersistedState, load_state, save_state};
use spectral_lib::state::{AppState, Status};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

fn main() {
    let state = Arc::new(AppState::new());
    let virtual_mic: Arc<Mutex<Option<VirtualMic>>> = Arc::new(Mutex::new(None));

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
        match VirtualMic::create(state.clone(), device_id, &persisted.mic_name) {
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

                let handle = thread::spawn(move || {
                    if let Err(e) = handle_client(&mut conn, state, virtual_mic) {
                        // Ignore errors after shutdown initiated
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
    // 8. virtual_mic
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

                        match VirtualMic::create(state.clone(), final_device_id, &final_mic_name) {
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

                        // Check if this matches the currently applied value
                        if threshold == applied_threshold {
                            // UI value matches applied - clear pending
                            *state
                                .pending_threshold
                                .lock()
                                .expect("pending_threshold mutex poisoned") = None;

                            // Check if all pending values are None
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
                            // UI value differs from applied - mark as pending
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
                        };
                        DaemonResponse::State(daemon_state)
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
