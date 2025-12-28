use spectral_lib::audio_devices;
use spectral_lib::ipc::{DaemonClient, DaemonCommand, DaemonResponse};
use spectral_lib::state::{AudioDevice, Status};

use slint::ModelRc;
use slint::StandardListViewItem;
use slint::VecModel;
use slint::Weak;

use std::error::Error;
use std::process::{Command, Stdio};
use std::rc::Rc;

slint::include_modules!();

fn filter_devices(devices: &[AudioDevice], mic_name: &str) -> Vec<AudioDevice> {
    let mic_name_hyphen = mic_name.replace(" ", "-");
    let mic_name_underscore = mic_name.replace(" ", "_");

    devices
        .iter()
        .filter(|device| {
            device.name != mic_name
                && device.name != mic_name_hyphen
                && device.name != mic_name_underscore
        })
        .cloned()
        .collect()
}

fn hex_to_color(hex: &str) -> slint::Color {
    let hex = hex.trim_start_matches('#');
    let r = u8::from_str_radix(&hex[0..2], 16).unwrap_or(0);
    let g = u8::from_str_radix(&hex[2..4], 16).unwrap_or(0);
    let b = u8::from_str_radix(&hex[4..6], 16).unwrap_or(0);
    slint::Color::from_rgb_u8(r, g, b)
}

fn update_ui_status(ui_weak: &Weak<MainWindow>, status: Status) {
    if let Some(ui) = ui_weak.upgrade() {
        let (color_str, status_text) = match status {
            Status::Pending => ("#FFFF00", "Status: Pending Changes"),
            Status::Active => ("#3FB950", "Status: Active"),
            Status::Inactive => ("#FF0000", "Status: Inactive"),
        };
        ui.set_status_color(hex_to_color(color_str));
        ui.set_status_text(status_text.into());
    }
}

fn start_daemon() -> Result<(), Box<dyn Error>> {
    let current_exe = std::env::current_exe()?;
    let daemon_dir = current_exe
        .parent()
        .ok_or("Failed to determine daemon directory (executable might be at root)")?;
    let daemon_path = daemon_dir.join("spectral-daemon");

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

    std::thread::sleep(std::time::Duration::from_millis(500));
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut daemon_client = match DaemonClient::connect() {
        Ok(client) => client,
        Err(_) => {
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

    let input_devices = audio_devices::get_input_devices()?;
    let filtered_devices = filter_devices(&input_devices, &daemon_state.mic_name);

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
        && let Some(idx) = filtered_devices.iter().position(|d| d.id == device_id)
    {
        ui.set_selected_input_index(idx as i32);
    }

    // Track applied values locally to determine pending state without IPC
    let applied_threshold = Rc::new(std::cell::Cell::new(daemon_state.threshold));
    let applied_device_id = Rc::new(std::cell::Cell::new(daemon_state.device_id));
    let applied_mic_name = Rc::new(std::cell::RefCell::new(daemon_state.mic_name.clone()));
    let current_status = Rc::new(std::cell::Cell::new(daemon_state.status));

    update_ui_status(&ui_weak, daemon_state.status);

    ui.on_selected_input_index_change({
        let ui_weak = ui_weak.clone();
        let devices = filtered_devices.clone();
        let applied_device_id = applied_device_id.clone();
        let applied_threshold = applied_threshold.clone();
        let applied_mic_name = applied_mic_name.clone();
        let current_status = current_status.clone();
        move |idx: i32| {
            if idx >= 0
                && (idx as usize) < devices.len()
                && let Some(ui) = ui_weak.upgrade()
            {
                let selected_device = devices[idx as usize].id;
                let device_matches = Some(selected_device) == applied_device_id.get();
                let threshold_matches = ui.get_slider_value() == applied_threshold.get();
                let mic_matches =
                    ui.get_mic_name_text().as_str() == applied_mic_name.borrow().as_str();

                let all_match = device_matches && threshold_matches && mic_matches;

                if current_status.get() == Status::Active && !all_match {
                    current_status.set(Status::Pending);
                    update_ui_status(&ui_weak, Status::Pending);
                } else if current_status.get() == Status::Pending && all_match {
                    current_status.set(Status::Active);
                    update_ui_status(&ui_weak, Status::Active);
                }
            }
        }
    });

    ui.on_threshold_change({
        let ui_weak = ui_weak.clone();
        let devices = filtered_devices.clone();
        let applied_threshold = applied_threshold.clone();
        let applied_device_id = applied_device_id.clone();
        let applied_mic_name = applied_mic_name.clone();
        let current_status = current_status.clone();
        move |val| {
            if let Some(ui) = ui_weak.upgrade() {
                let threshold_matches = val == applied_threshold.get();
                let mic_matches =
                    ui.get_mic_name_text().as_str() == applied_mic_name.borrow().as_str();
                let idx = ui.get_selected_input_index();
                let device_matches = if idx >= 0 && (idx as usize) < devices.len() {
                    Some(devices[idx as usize].id) == applied_device_id.get()
                } else {
                    applied_device_id.get().is_none()
                };

                let all_match = device_matches && threshold_matches && mic_matches;

                if current_status.get() == Status::Active && !all_match {
                    current_status.set(Status::Pending);
                    update_ui_status(&ui_weak, Status::Pending);
                } else if current_status.get() == Status::Pending && all_match {
                    current_status.set(Status::Active);
                    update_ui_status(&ui_weak, Status::Active);
                }
            }
        }
    });

    ui.on_rename_clicked({
        let ui_weak = ui_weak.clone();
        let devices = filtered_devices.clone();
        let applied_mic_name = applied_mic_name.clone();
        let applied_threshold = applied_threshold.clone();
        let applied_device_id = applied_device_id.clone();
        let current_status = current_status.clone();
        move |new_name| {
            if let Some(ui) = ui_weak.upgrade() {
                let new_name_str = new_name.to_string();
                let mic_matches = new_name_str == *applied_mic_name.borrow();
                let threshold_matches = ui.get_slider_value() == applied_threshold.get();
                let idx = ui.get_selected_input_index();
                let device_matches = if idx >= 0 && (idx as usize) < devices.len() {
                    Some(devices[idx as usize].id) == applied_device_id.get()
                } else {
                    applied_device_id.get().is_none()
                };

                let all_match = device_matches && threshold_matches && mic_matches;

                if current_status.get() == Status::Active && !all_match {
                    current_status.set(Status::Pending);
                    update_ui_status(&ui_weak, Status::Pending);
                } else if current_status.get() == Status::Pending && all_match {
                    current_status.set(Status::Active);
                    update_ui_status(&ui_weak, Status::Active);
                }
            }
        }
    });

    ui.on_reset_clicked({
        let ui_weak = ui_weak.clone();
        let devices = filtered_devices.clone();
        let applied_mic_name = applied_mic_name.clone();
        let applied_threshold = applied_threshold.clone();
        let applied_device_id = applied_device_id.clone();
        let current_status = current_status.clone();
        move |default_name| {
            if let Some(ui) = ui_weak.upgrade() {
                let default_name_str = default_name.to_string();
                let mic_matches = default_name_str == *applied_mic_name.borrow();
                let threshold_matches = ui.get_slider_value() == applied_threshold.get();
                let idx = ui.get_selected_input_index();
                let device_matches = if idx >= 0 && (idx as usize) < devices.len() {
                    Some(devices[idx as usize].id) == applied_device_id.get()
                } else {
                    applied_device_id.get().is_none()
                };

                let all_match = device_matches && threshold_matches && mic_matches;

                if current_status.get() == Status::Active && !all_match {
                    current_status.set(Status::Pending);
                    update_ui_status(&ui_weak, Status::Pending);
                } else if current_status.get() == Status::Pending && all_match {
                    current_status.set(Status::Active);
                    update_ui_status(&ui_weak, Status::Active);
                }
            }
        }
    });

    ui.on_enable_clicked({
        let ui_weak = ui_weak.clone();
        let devices = filtered_devices.clone();
        let applied_threshold = applied_threshold.clone();
        let applied_device_id = applied_device_id.clone();
        let applied_mic_name = applied_mic_name.clone();
        let current_status = current_status.clone();
        move || {
            if let Some(ui) = ui_weak.upgrade() {
                let idx = ui.get_selected_input_index();
                let threshold = ui.get_slider_value();
                let mic_name = ui.get_mic_name_text().to_string();

                if idx >= 0
                    && (idx as usize) < devices.len()
                    && let Ok(ref mut c) = DaemonClient::connect()
                {
                    let device_id = devices[idx as usize].id;
                    match c.send_command(DaemonCommand::Enable {
                        device_id,
                        mic_name: mic_name.clone(),
                        threshold,
                    }) {
                        Ok(DaemonResponse::Ok) => {
                            applied_threshold.set(threshold);
                            applied_device_id.set(Some(device_id));
                            *applied_mic_name.borrow_mut() = mic_name;
                            current_status.set(Status::Active);
                            update_ui_status(&ui_weak, Status::Active);
                        }
                        Ok(DaemonResponse::Error(e)) => {
                            eprintln!("Failed to enable: {}", e);
                            current_status.set(Status::Inactive);
                            update_ui_status(&ui_weak, Status::Inactive);
                        }
                        _ => {
                            current_status.set(Status::Inactive);
                            update_ui_status(&ui_weak, Status::Inactive);
                        }
                    }
                }
            }
        }
    });

    ui.on_disable_clicked({
        let ui_weak = ui_weak.clone();
        let current_status = current_status.clone();
        move || {
            if let Ok(ref mut c) = DaemonClient::connect() {
                let _ = c.send_command(DaemonCommand::Disable);
                current_status.set(Status::Inactive);
                update_ui_status(&ui_weak, Status::Inactive);
            }
        }
    });

    let ui_weak_clone = ui_weak.clone();
    ui.on_mic_name_text_changed(move |text| {
        let text_str = text.to_string();
        if text_str.len() > 21
            && let Some(ui) = ui_weak_clone.upgrade()
        {
            ui.set_mic_name_text(text_str.chars().take(21).collect::<String>().into());
        }
    });

    ui.on_refresh_devices_clicked({
        let ui_weak = ui_weak.clone();
        move || {
            if let Some(ui) = ui_weak.upgrade() {
                match audio_devices::get_input_devices() {
                    Ok(fresh_devices) => {
                        let mic_name = DaemonClient::connect()
                            .ok()
                            .and_then(|mut c| {
                                c.send_command(DaemonCommand::GetState).ok().and_then(|r| {
                                    if let DaemonResponse::State(state) = r {
                                        Some(state.mic_name)
                                    } else {
                                        None
                                    }
                                })
                            })
                            .unwrap_or_else(|| "Spectral-Mic".to_string());

                        let filtered = filter_devices(&fresh_devices, &mic_name);

                        let items: Vec<StandardListViewItem> = filtered
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
                        if selected_idx >= 0 && (selected_idx as usize) < filtered.len() {
                            ui.set_selected_input_index(selected_idx);
                        } else {
                            ui.set_selected_input_index(-1);
                        }
                    }
                    Err(e) => {
                        eprintln!("Failed to refresh devices: {}", e);
                    }
                }
            }
        }
    });

    ui.run()?;

    Ok(())
}
