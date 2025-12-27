use spectral_lib::audio_devices_linux;
use spectral_lib::ipc::{DaemonClient, DaemonCommand, DaemonResponse};

use slint::ModelRc;
use slint::StandardListViewItem;
use slint::VecModel;
use slint::Weak;

use std::error::Error;
use std::process::{Command, Stdio};
use std::rc::Rc;

slint::include_modules!();

fn start_daemon() -> Result<(), Box<dyn Error>> {
    let daemon_path = std::env::current_exe()?
        .parent()
        .unwrap()
        .join("spectral-daemon");

    let log_path = std::env::var("XDG_RUNTIME_DIR")
        .or_else(|_| std::env::var("HOME").map(|h| format!("{}/tmp", h)))
        .unwrap_or_else(|_| "/tmp".to_string());

    let log_file = std::path::PathBuf::from(log_path).join("spectral-daemon.log");
    let log = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&log_file)?;

    Command::new(&daemon_path)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .spawn()?;

    println!("Daemon started. Logs: {}", log_file.display());
    std::thread::sleep(std::time::Duration::from_millis(500));
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut daemon_client = match DaemonClient::connect() {
        Ok(client) => client,
        Err(_) => {
            println!("Daemon not running, starting daemon...");
            start_daemon()?;
            DaemonClient::connect()?
        }
    };

    let daemon_state = match daemon_client.send_command(DaemonCommand::GetState) {
        Ok(DaemonResponse::State(state)) => state,
        Ok(_) => return Err("Unexpected response from daemon".into()),
        Err(e) => {
            eprintln!(
                "Failed to communicate with daemon (possibly old version), restarting: {}",
                e
            );
            let _ = daemon_client.send_command(DaemonCommand::Shutdown);
            std::thread::sleep(std::time::Duration::from_millis(300));
            start_daemon()?;

            let mut new_client = DaemonClient::connect()?;
            match new_client.send_command(DaemonCommand::GetState) {
                Ok(DaemonResponse::State(state)) => state,
                _ => return Err("Failed to get daemon state after restart".into()),
            }
        }
    };

    let ui = MainWindow::new()?;
    let ui_weak = ui.as_weak();

    let input_devices = audio_devices_linux::get_input_devices()?;

    let filtered_devices: Vec<_> = input_devices
        .iter()
        .filter(|device| {
            let mic_name_hyphen = daemon_state.mic_name.replace(" ", "-");
            let mic_name_underscore = daemon_state.mic_name.replace(" ", "_");
            let device_name = &device.name;

            device_name != &daemon_state.mic_name
                && device_name != &mic_name_hyphen
                && device_name != &mic_name_underscore
        })
        .cloned()
        .collect();

    let mut input_devices_vec: Vec<StandardListViewItem> = filtered_devices
        .iter()
        .map(|device| {
            let mut item = StandardListViewItem::default();
            item.text = device.name.clone().into();
            item
        })
        .collect();

    if input_devices_vec.is_empty() {
        let mut item = StandardListViewItem::default();
        item.text = "No input devices detected".into();
        input_devices_vec.push(item);
    }

    let vec_model = VecModel::from(input_devices_vec);
    let model_rc: ModelRc<StandardListViewItem> = ModelRc::new(Rc::new(vec_model));
    ui.set_input_devices(model_rc);

    ui.set_mic_name_text(daemon_state.mic_name.clone().into());
    ui.set_slider_value(daemon_state.threshold);

    if let Some(device_id) = daemon_state.device_id
        && let Some(idx) = filtered_devices.iter().position(|d| d.id == device_id) {
            ui.set_selected_input_index(idx as i32);
        }

    fn hex_to_color(hex: &str) -> slint::Color {
        let hex = hex.trim_start_matches('#');
        assert_eq!(hex.len(), 6, "Invalid hex color length");

        let r = u8::from_str_radix(&hex[0..2], 16).expect("Invalid red component");
        let g = u8::from_str_radix(&hex[2..4], 16).expect("Invalid green component");
        let b = u8::from_str_radix(&hex[4..6], 16).expect("Invalid blue component");

        slint::Color::from_rgb_u8(r, g, b)
    }

    fn update_ui_status(ui_weak: &Weak<MainWindow>, is_active: bool, is_pending: bool) {
        if let Some(ui) = ui_weak.upgrade() {
            let (color_str, status_text) = if is_pending {
                ("#FFFF00", "Status: Pending Changes")
            } else if is_active {
                ("#3FB950", "Status: Active")
            } else {
                ("#FF0000", "Status: Inactive")
            };
            ui.set_status_color(hex_to_color(color_str));
            ui.set_status_text(status_text.into());
        }
    }

    let is_active = daemon_state.status == "Active";
    let is_pending = daemon_state.status == "Pending";
    update_ui_status(&ui_weak, is_active, is_pending);

    ui.on_selected_input_index_change({
        let ui_weak = ui_weak.clone();
        let devices = filtered_devices.clone();
        move |idx: i32| {
            let device_id = if idx >= 0 && (idx as usize) < devices.len() {
                Some(devices[idx as usize].id)
            } else {
                None
            };

            if let Some(id) = device_id {
                let mut client = DaemonClient::connect().ok();
                if let Some(ref mut c) = client {
                    let _ = c.send_command(DaemonCommand::UpdateDevice(id));

                    if let Ok(DaemonResponse::State(state)) =
                        c.send_command(DaemonCommand::GetState)
                    {
                        let is_active = state.status == "Active";
                        let is_pending = state.status == "Pending";
                        update_ui_status(&ui_weak, is_active, is_pending);
                    }
                }
            }
        }
    });

    ui.on_threshold_change({
        let ui_weak = ui_weak.clone();
        move |val| {
            let mut client = DaemonClient::connect().ok();
            if let Some(ref mut c) = client {
                let _ = c.send_command(DaemonCommand::UpdateThreshold(val));

                if let Ok(DaemonResponse::State(state)) = c.send_command(DaemonCommand::GetState) {
                    let is_active = state.status == "Active";
                    let is_pending = state.status == "Pending";
                    update_ui_status(&ui_weak, is_active, is_pending);
                }
            }
        }
    });

    ui.on_rename_clicked({
        let ui_weak = ui_weak.clone();
        move |new_name| {
            let mut client = DaemonClient::connect().ok();
            if let Some(ref mut c) = client {
                let _ = c.send_command(DaemonCommand::UpdateMicName(new_name.to_string()));

                if let Ok(DaemonResponse::State(state)) = c.send_command(DaemonCommand::GetState) {
                    let is_active = state.status == "Active";
                    let is_pending = state.status == "Pending";
                    update_ui_status(&ui_weak, is_active, is_pending);
                }
            }
        }
    });

    ui.on_reset_clicked({
        let ui_weak = ui_weak.clone();
        move |default_name| {
            let mut client = DaemonClient::connect().ok();
            if let Some(ref mut c) = client {
                let _ = c.send_command(DaemonCommand::UpdateMicName(default_name.to_string()));

                if let Ok(DaemonResponse::State(state)) = c.send_command(DaemonCommand::GetState) {
                    let is_active = state.status == "Active";
                    let is_pending = state.status == "Pending";
                    update_ui_status(&ui_weak, is_active, is_pending);
                }
            }
        }
    });

    ui.on_enable_clicked({
        let ui_weak = ui_weak.clone();
        let devices = filtered_devices.clone();
        move || {
            if let Some(ui) = ui_weak.upgrade() {
                let idx = ui.get_selected_input_index();
                let threshold = ui.get_slider_value();
                let mic_name = ui.get_mic_name_text().to_string();

                if idx >= 0 && (idx as usize) < devices.len() {
                    let device_id = devices[idx as usize].id;

                    let mut client = DaemonClient::connect().ok();
                    if let Some(ref mut c) = client {
                        match c.send_command(DaemonCommand::Enable {
                            device_id,
                            mic_name,
                            threshold,
                        }) {
                            Ok(DaemonResponse::Ok) => {
                                update_ui_status(&ui_weak, true, false);
                            }
                            Ok(DaemonResponse::Error(e)) => {
                                eprintln!("Failed to enable: {}", e);
                                update_ui_status(&ui_weak, false, false);
                            }
                            _ => {
                                update_ui_status(&ui_weak, false, false);
                            }
                        }
                    }
                }
            }
        }
    });

    ui.on_disable_clicked({
        let ui_weak = ui_weak.clone();
        move || {
            let mut client = DaemonClient::connect().ok();
            if let Some(ref mut c) = client {
                let _ = c.send_command(DaemonCommand::Disable);
                update_ui_status(&ui_weak, false, false);
            }
        }
    });

    let ui_weak_clone = ui_weak.clone();
    ui.on_mic_name_text_changed(move |text| {
        let text_str = text.to_string();
        if text_str.len() > 21
            && let Some(ui) = ui_weak_clone.upgrade() {
                ui.set_mic_name_text(text_str.chars().take(21).collect::<String>().into());
            }
    });

    ui.on_refresh_devices_clicked({
        let ui_weak = ui_weak.clone();
        move || {
            if let Some(ui) = ui_weak.upgrade() {
                match audio_devices_linux::get_input_devices() {
                    Ok(fresh_devices) => {
                        let mut client = DaemonClient::connect().ok();
                        let mic_name = if let Some(ref mut c) = client {
                            match c.send_command(DaemonCommand::GetState) {
                                Ok(DaemonResponse::State(state)) => state.mic_name,
                                _ => "Spectral-Mic".to_string(),
                            }
                        } else {
                            "Spectral-Mic".to_string()
                        };

                        let filtered_devices: Vec<_> = fresh_devices
                            .iter()
                            .filter(|device| {
                                let mic_name_hyphen = mic_name.replace(" ", "-");
                                let mic_name_underscore = mic_name.replace(" ", "_");
                                let device_name = &device.name;

                                device_name != &mic_name
                                    && device_name != &mic_name_hyphen
                                    && device_name != &mic_name_underscore
                            })
                            .cloned()
                            .collect();

                        let items: Vec<StandardListViewItem> = filtered_devices
                            .iter()
                            .map(|device| {
                                let mut item = StandardListViewItem::default();
                                item.text = device.name.clone().into();
                                item
                            })
                            .collect();

                        let model = VecModel::from(items);
                        ui.set_input_devices(ModelRc::new(Rc::new(model)));

                        let selected_idx = ui.get_selected_input_index();
                        if selected_idx >= 0 && (selected_idx as usize) < filtered_devices.len() {
                            ui.set_selected_input_index(selected_idx);
                        } else {
                            ui.set_selected_input_index(-1);
                        }

                        println!("[Refresh] Device list updated");
                    }
                    Err(e) => {
                        eprintln!("[Refresh] Failed to refresh devices: {}", e);
                    }
                }
            }
        }
    });

    ui.run()?;

    Ok(())
}
