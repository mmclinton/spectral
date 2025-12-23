use spectral_lib::audio_processor_linux::VirtualMic;
use spectral_lib::ipc::{DaemonCommand, DaemonResponse, DaemonServer, DaemonState};
use spectral_lib::persistence::{PersistedState, load_state, save_state};
use spectral_lib::state::{AppState, Status};
use std::sync::{Arc, Mutex};
use std::thread;

fn main() {
    println!("Spectral Daemon starting...");

    let state = Arc::new(AppState::new());
    let virtual_mic: Arc<Mutex<Option<VirtualMic>>> = Arc::new(Mutex::new(None));

    let persisted = load_state().unwrap_or_default();
    {
        *state.threshold_val.lock().unwrap() = persisted.threshold;
        *state.mic_name.lock().unwrap() = persisted.mic_name.clone();
        *state.selected_device_id.lock().unwrap() = persisted.device_id;
    }

    if persisted.is_active {
        if let Some(device_id) = persisted.device_id {
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
    }

    let server = match DaemonServer::bind() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to bind daemon socket: {}", e);
            return;
        }
    };

    println!("Daemon listening for connections...");

    loop {
        match server.accept() {
            Ok(mut conn) => {
                let state = state.clone();
                let virtual_mic = virtual_mic.clone();

                thread::spawn(move || {
                    if let Err(e) = handle_client(&mut conn, state, virtual_mic) {
                        eprintln!("Client handler error: {}", e);
                    }
                });
            }
            Err(e) => {
                eprintln!("Failed to accept connection: {}", e);
            }
        }
    }
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
                        *state.threshold_val.lock().unwrap() = threshold;
                        *state.mic_name.lock().unwrap() = mic_name.clone();
                        *state.selected_device_id.lock().unwrap() = Some(device_id);

                        let mut mic_guard = virtual_mic.lock().unwrap();
                        if let Some(mic) = mic_guard.take() {
                            mic.stop();
                        }

                        match VirtualMic::create(state.clone(), device_id, &mic_name) {
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
                        let old_threshold = *state.threshold_val.lock().unwrap();
                        if old_threshold != threshold {
                            *state.threshold_val.lock().unwrap() = threshold;

                            let status = state.status.lock().unwrap().clone();
                            if let Status::Active = status {
                                state.update_status(Status::Pending);
                            }
                        }
                        DaemonResponse::Ok
                    }
                    DaemonCommand::UpdateMicName(mic_name) => {
                        let old_name = state.mic_name.lock().unwrap().clone();
                        if old_name != mic_name {
                            *state.mic_name.lock().unwrap() = mic_name;

                            let status = state.status.lock().unwrap().clone();
                            if let Status::Active = status {
                                state.update_status(Status::Pending);
                            }
                        }
                        DaemonResponse::Ok
                    }
                    DaemonCommand::UpdateDevice(device_id) => {
                        let old_id = *state.selected_device_id.lock().unwrap();
                        if old_id != Some(device_id) {
                            *state.selected_device_id.lock().unwrap() = Some(device_id);

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
