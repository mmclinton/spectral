use spectral_lib::audio_processor_linux::VirtualMic;
use spectral_lib::ipc::{DaemonCommand, DaemonResponse, DaemonServer, DaemonState};
use spectral_lib::persistence::{PersistedState, load_state, save_state};
use spectral_lib::state::{AppState, Status};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

fn main() {
    println!("Spectral Daemon starting...");

    let state = Arc::new(AppState::new());
    let virtual_mic: Arc<Mutex<Option<VirtualMic>>> = Arc::new(Mutex::new(None));
    
    // Set up graceful shutdown flag
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();

    // Set up signal handlers for graceful shutdown
    let mut signals = signal_hook::iterator::Signals::new(&[
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGINT,
    ]).expect("Failed to register signal handlers");
    
    let signal_thread = thread::spawn(move || {
        for sig in signals.forever() {
            eprintln!("Received signal {:?}, initiating graceful shutdown...", sig);
            r.store(false, Ordering::SeqCst);
            break;
        }
    });

    let persisted = load_state().unwrap_or_default();
    {
        *state.threshold_val.lock().unwrap() = persisted.threshold;
        *state.mic_name.lock().unwrap() = persisted.mic_name.clone();
        *state.selected_device_id.lock().unwrap() = persisted.device_id;
    }

    if persisted.is_active
        && let Some(device_id) = persisted.device_id {
            println!(
                "Restoring previous session: device_id={}, mic_name={}, threshold={}",
                device_id, persisted.mic_name, persisted.threshold
            );
            match VirtualMic::create(state.clone(), device_id, &persisted.mic_name) {
                Ok(mic) => {
                    *virtual_mic.lock().unwrap() = Some(mic);
                    state.update_status(Status::Active);
                    println!("Virtual mic restored successfully");
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

    println!("Daemon listening for connections...");
    
    // Track client handler threads for proper cleanup
    let mut client_threads: Vec<thread::JoinHandle<()>> = Vec::new();

    loop {
        // Check if we should shutdown
        if !running.load(Ordering::SeqCst) {
            break;
        }
        
        // Clean up finished threads periodically
        client_threads.retain(|handle| !handle.is_finished());
        
        // Use non-blocking accept by checking with a timeout pattern
        // Note: UnixListener doesn't support set_nonblocking in all cases,
        // so we spawn threads but track them properly
        match server.accept() {
            Ok(mut conn) => {
                let state = state.clone();
                let virtual_mic = virtual_mic.clone();
                
                let handle = thread::spawn(move || {
                    if let Err(e) = handle_client(&mut conn, state, virtual_mic) {
                        eprintln!("Client handler error: {}", e);
                    }
                });
                
                client_threads.push(handle);
            }
            Err(e) => {
                // Only log actual errors, not timeout/interrupt
                if e.kind() != std::io::ErrorKind::Interrupted {
                    eprintln!("Failed to accept connection: {}", e);
                }
                // Small sleep to prevent busy loop on errors
                thread::sleep(Duration::from_millis(100));
            }
        }
    }
    
    // Graceful shutdown sequence
    println!("Shutting down gracefully...");
    
    // Stop virtual mic if active
    if let Some(mic) = virtual_mic.lock().unwrap().take() {
        mic.stop();
    }
    
    // Save state
    let persisted = PersistedState {
        is_active: false,
        device_id: *state.selected_device_id.lock().unwrap(),
        mic_name: state.mic_name.lock().unwrap().clone(),
        threshold: *state.threshold_val.lock().unwrap(),
    };
    let _ = save_state(&persisted);
    
    // Wait for client threads to finish (with timeout)
    println!("Waiting for client threads to finish...");
    for handle in client_threads {
        // Join with a reasonable timeout concept (Rust's JoinHandle doesn't have timeout,
        // but threads should finish quickly after connections close)
        let _ = handle.join();
    }
    
    // Wait for signal thread
    let _ = signal_thread.join();
    
    println!("Daemon shutdown complete");
}

fn handle_client(
    conn: &mut spectral_lib::ipc::DaemonConnection,
    state: Arc<AppState>,
    virtual_mic: Arc<Mutex<Option<VirtualMic>>>,
) -> Result<(), std::io::Error> {
    loop {
        match conn.read_command() {
            Ok(command) => {
                println!("[Daemon] Received command: {:?}", command);
                let response = match command {
                    DaemonCommand::Enable {
                        device_id,
                        mic_name,
                        threshold,
                    } => {
                        // Apply all values (both from pending and direct)
                        let final_threshold =
                            state.pending_threshold.lock().unwrap().unwrap_or(threshold);
                        let final_device_id =
                            state.pending_device_id.lock().unwrap().unwrap_or(device_id);
                        let final_mic_name = state
                            .pending_mic_name
                            .lock()
                            .unwrap()
                            .clone()
                            .unwrap_or(mic_name.clone());

                        *state.threshold_val.lock().unwrap() = final_threshold;
                        *state.mic_name.lock().unwrap() = final_mic_name.clone();
                        *state.selected_device_id.lock().unwrap() = Some(final_device_id);

                        // Clear all pending values
                        *state.pending_threshold.lock().unwrap() = None;
                        *state.pending_device_id.lock().unwrap() = None;
                        *state.pending_mic_name.lock().unwrap() = None;

                        let mut mic_guard = virtual_mic.lock().unwrap();
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
                        let mut mic_guard = virtual_mic.lock().unwrap();
                        if let Some(mic) = mic_guard.take() {
                            mic.stop();
                        }
                        state.update_status(Status::Inactive);
                        DaemonResponse::Ok
                    }
                    DaemonCommand::UpdateThreshold(threshold) => {
                        let applied_threshold = *state.threshold_val.lock().unwrap();

                        // Check if this matches the currently applied value
                        if threshold == applied_threshold {
                            // UI value matches applied - clear pending
                            *state.pending_threshold.lock().unwrap() = None;

                            // Check if all pending values are None
                            let has_pending_device =
                                state.pending_device_id.lock().unwrap().is_some();
                            let has_pending_name = state.pending_mic_name.lock().unwrap().is_some();

                            if !has_pending_device && !has_pending_name {
                                let status = state.status.lock().unwrap().clone();
                                if let Status::Pending = status {
                                    state.update_status(Status::Active);
                                }
                            }
                        } else {
                            // UI value differs from applied - mark as pending
                            *state.pending_threshold.lock().unwrap() = Some(threshold);

                            let status = state.status.lock().unwrap().clone();
                            if let Status::Active = status {
                                state.update_status(Status::Pending);
                            }
                        }
                        DaemonResponse::Ok
                    }
                    DaemonCommand::UpdateMicName(mic_name) => {
                        let applied_name = state.mic_name.lock().unwrap().clone();

                        if mic_name == applied_name {
                            *state.pending_mic_name.lock().unwrap() = None;

                            let has_pending_threshold =
                                state.pending_threshold.lock().unwrap().is_some();
                            let has_pending_device =
                                state.pending_device_id.lock().unwrap().is_some();

                            if !has_pending_threshold && !has_pending_device {
                                let status = state.status.lock().unwrap().clone();
                                if let Status::Pending = status {
                                    state.update_status(Status::Active);
                                }
                            }
                        } else {
                            *state.pending_mic_name.lock().unwrap() = Some(mic_name);

                            let status = state.status.lock().unwrap().clone();
                            if let Status::Active = status {
                                state.update_status(Status::Pending);
                            }
                        }
                        DaemonResponse::Ok
                    }
                    DaemonCommand::UpdateDevice(device_id) => {
                        let applied_device = *state.selected_device_id.lock().unwrap();

                        if Some(device_id) == applied_device {
                            *state.pending_device_id.lock().unwrap() = None;

                            let has_pending_threshold =
                                state.pending_threshold.lock().unwrap().is_some();
                            let has_pending_name = state.pending_mic_name.lock().unwrap().is_some();

                            if !has_pending_threshold && !has_pending_name {
                                let status = state.status.lock().unwrap().clone();
                                if let Status::Pending = status {
                                    state.update_status(Status::Active);
                                }
                            }
                        } else {
                            *state.pending_device_id.lock().unwrap() = Some(device_id);

                            let status = state.status.lock().unwrap().clone();
                            if let Status::Active = status {
                                state.update_status(Status::Pending);
                            }
                        }
                        DaemonResponse::Ok
                    }
                    DaemonCommand::GetState => {
                        let status = state.status.lock().unwrap().clone();
                        let status_str = match status {
                            Status::Inactive => "Inactive",
                            Status::Active => "Active",
                            Status::Pending => "Pending",
                        };
                        let daemon_state = DaemonState {
                            status: status_str.to_string(),
                            device_id: *state.selected_device_id.lock().unwrap(),
                            mic_name: state.mic_name.lock().unwrap().clone(),
                            threshold: *state.threshold_val.lock().unwrap(),
                        };
                        DaemonResponse::State(daemon_state)
                    }
                    DaemonCommand::Shutdown => {
                        let mut mic_guard = virtual_mic.lock().unwrap();
                        if let Some(mic) = mic_guard.take() {
                            mic.stop();
                        }

                        let persisted = PersistedState {
                            is_active: false,
                            device_id: *state.selected_device_id.lock().unwrap(),
                            mic_name: state.mic_name.lock().unwrap().clone(),
                            threshold: *state.threshold_val.lock().unwrap(),
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
