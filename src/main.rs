use spectral_lib::audio_devices;
use spectral_lib::ipc::{DaemonClient, DaemonCommand, DaemonResponse, wait_for_daemon_exit};
use spectral_lib::state::{AudioDevice, MIC_NAME_MAX_LEN, Status};

use slint::ModelRc;
use slint::StandardListViewItem;
use slint::VecModel;
use slint::Weak;

use std::error::Error;
use std::process::{Command, Stdio};
use std::rc::Rc;

slint::include_modules!();

fn is_valid_mic_name_input(text: &str) -> bool {
    text.chars().count() <= MIC_NAME_MAX_LEN
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn filter_devices(devices: &[AudioDevice], mic_name: &str) -> Vec<AudioDevice> {
    devices
        .iter()
        .filter(|device| device.name != mic_name)
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

/// Whether the device at `idx` in the filtered list equals the applied device,
/// treating an out-of-range index as "no device" (matches only when none is
/// applied). The settings-change callbacks share this comparison.
fn device_matches_applied(
    idx: i32,
    devices: &[AudioDevice],
    applied_device_id: &std::cell::Cell<Option<u32>>,
) -> bool {
    if idx >= 0 && (idx as usize) < devices.len() {
        Some(devices[idx as usize].id) == applied_device_id.get()
    } else {
        applied_device_id.get().is_none()
    }
}

/// Drive the Active↔Pending transition shared by the settings-change callbacks:
/// an applied (Active) mic drifts to Pending when settings diverge, and back to
/// Active when they realign. Any other state is left untouched.
fn apply_pending_transition(
    current_status: &std::cell::Cell<Status>,
    ui_weak: &Weak<MainWindow>,
    all_match: bool,
) {
    match current_status.get() {
        Status::Active if !all_match => {
            current_status.set(Status::Pending);
            update_ui_status(ui_weak, Status::Pending);
        }
        Status::Pending if all_match => {
            current_status.set(Status::Active);
            update_ui_status(ui_weak, Status::Active);
        }
        _ => {}
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

    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    unsafe { std::env::set_var("SLINT_BACKEND", "winit-software") };

    run_app()
}

fn run_app() -> Result<(), Box<dyn Error>> {
    let mut daemon_client = match DaemonClient::connect() {
        Ok(client) => client,
        Err(_) => {
            start_daemon()?;
            DaemonClient::connect_with_retry()?
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
            wait_for_daemon_exit()?;
            start_daemon()?;

            let mut new_client = DaemonClient::connect_with_retry()?;
            match new_client.send_command(DaemonCommand::GetState) {
                Ok(DaemonResponse::State(state)) => state,
                _ => return Err("Failed to get daemon state after restart".into()),
            }
        }
    };

    let ui = MainWindow::new()?;
    // Must come after MainWindow::new(): until the Slint platform context exists,
    // set_xdg_app_id returns Err and stores nothing, leaving the Wayland surface
    // without an app_id so the desktop file (and taskbar icon) can't be matched.
    if let Err(e) = slint::set_xdg_app_id("page.codeberg.mmc.Spectral") {
        eprintln!("warning: failed to set xdg app id (taskbar icon may be generic): {e}");
    }
    let ui_weak = ui.as_weak();

    let input_devices = audio_devices::get_input_devices()?;
    let filtered_devices = Rc::new(std::cell::RefCell::new(filter_devices(
        &input_devices,
        &daemon_state.mic_name,
    )));

    let mut input_devices_vec: Vec<StandardListViewItem> = filtered_devices
        .borrow()
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

    let last_valid_mic_name = Rc::new(std::cell::RefCell::new(daemon_state.mic_name.clone()));

    if let Some(device_id) = daemon_state.device_id
        && let Some(idx) = filtered_devices
            .borrow()
            .iter()
            .position(|d| d.id == device_id)
    {
        ui.set_selected_input_index(idx as i32);
    }

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
            let devices = devices.borrow();
            if idx >= 0
                && (idx as usize) < devices.len()
                && let Some(ui) = ui_weak.upgrade()
            {
                let device_matches = device_matches_applied(idx, &devices, &applied_device_id);
                let threshold_matches = ui.get_slider_value() == applied_threshold.get();
                let mic_matches =
                    ui.get_mic_name_text().as_str() == applied_mic_name.borrow().as_str();

                let all_match = device_matches && threshold_matches && mic_matches;
                apply_pending_transition(&current_status, &ui_weak, all_match);
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
            let devices = devices.borrow();
            if let Some(ui) = ui_weak.upgrade() {
                let threshold_matches = val == applied_threshold.get();
                let mic_matches =
                    ui.get_mic_name_text().as_str() == applied_mic_name.borrow().as_str();
                let idx = ui.get_selected_input_index();
                let device_matches = device_matches_applied(idx, &devices, &applied_device_id);

                let all_match = device_matches && threshold_matches && mic_matches;
                apply_pending_transition(&current_status, &ui_weak, all_match);
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
            let devices = devices.borrow();
            if let Some(ui) = ui_weak.upgrade() {
                let new_name_str = new_name.to_string();
                let mic_matches = new_name_str == *applied_mic_name.borrow();
                let threshold_matches = ui.get_slider_value() == applied_threshold.get();
                let idx = ui.get_selected_input_index();
                let device_matches = device_matches_applied(idx, &devices, &applied_device_id);

                let all_match = device_matches && threshold_matches && mic_matches;
                apply_pending_transition(&current_status, &ui_weak, all_match);
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
        let last_valid = last_valid_mic_name.clone();
        move |default_name| {
            let devices = devices.borrow();
            if let Some(ui) = ui_weak.upgrade() {
                let default_name_str = default_name.to_string();
                *last_valid.borrow_mut() = default_name_str.clone();
                let mic_matches = default_name_str == *applied_mic_name.borrow();
                let threshold_matches = ui.get_slider_value() == applied_threshold.get();
                let idx = ui.get_selected_input_index();
                let device_matches = device_matches_applied(idx, &devices, &applied_device_id);

                let all_match = device_matches && threshold_matches && mic_matches;
                apply_pending_transition(&current_status, &ui_weak, all_match);
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
            let devices = devices.borrow();
            if let Some(ui) = ui_weak.upgrade() {
                let idx = ui.get_selected_input_index();
                let threshold = ui.get_slider_value();
                let mic_name = ui.get_mic_name_text().to_string();
                if mic_name.is_empty() {
                    eprintln!("Cannot enable: mic name is empty");
                    return;
                }

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
    let last_valid = last_valid_mic_name.clone();
    ui.on_mic_name_text_changed(move |text| {
        if is_valid_mic_name_input(&text) {
            *last_valid.borrow_mut() = text.to_string();
            return;
        }
        let revert_to = last_valid.borrow().clone();
        let ui_weak = ui_weak_clone.clone();
        slint::invoke_from_event_loop(move || {
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_mic_name_text(revert_to.into());
                ui.set_mic_name_invalid_flash(true);
            }
        })
        .ok();
    });

    ui.on_refresh_devices_clicked({
        let ui_weak = ui_weak.clone();
        let devices = filtered_devices.clone();
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

                        *devices.borrow_mut() = filtered;
                    }
                    Err(e) => {
                        eprintln!("Failed to refresh devices: {}", e);
                    }
                }
            }
        }
    });

    ui.on_open_url(|url| {
        let url_str = url.to_string();
        if let Err(e) = Command::new("xdg-open")
            .arg(&url_str)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            eprintln!("Failed to open URL '{}': {}", url_str, e);
        }
    });

    ui.on_start_recording(|| {
        if let Ok(ref mut c) = DaemonClient::connect() {
            match c.send_command(DaemonCommand::StartRecording) {
                Ok(DaemonResponse::Ok) => {}
                Ok(DaemonResponse::Error(e)) => eprintln!("Failed to start recording: {}", e),
                _ => eprintln!("Unexpected response from daemon"),
            }
        }
    });

    ui.on_stop_recording(|| {
        if let Ok(ref mut c) = DaemonClient::connect() {
            match c.send_command(DaemonCommand::StopRecording) {
                Ok(DaemonResponse::Ok) => {}
                Ok(DaemonResponse::Error(e)) => eprintln!("Failed to stop recording: {}", e),
                _ => eprintln!("Unexpected response from daemon"),
            }
        }
    });

    ui.on_play_original(|| {
        if let Ok(ref mut c) = DaemonClient::connect() {
            match c.send_command(DaemonCommand::PlayOriginal) {
                Ok(DaemonResponse::Ok) => {}
                Ok(DaemonResponse::Error(e)) => eprintln!("Failed to play original: {}", e),
                _ => eprintln!("Unexpected response from daemon"),
            }
        }
    });

    ui.on_stop_original(|| {
        if let Ok(ref mut c) = DaemonClient::connect() {
            match c.send_command(DaemonCommand::StopOriginal) {
                Ok(DaemonResponse::Ok) => {}
                Ok(DaemonResponse::Error(e)) => eprintln!("Failed to stop original: {}", e),
                _ => eprintln!("Unexpected response from daemon"),
            }
        }
    });

    ui.on_play_filtered(|| {
        if let Ok(ref mut c) = DaemonClient::connect() {
            match c.send_command(DaemonCommand::PlayFiltered) {
                Ok(DaemonResponse::Ok) => {}
                Ok(DaemonResponse::Error(e)) => eprintln!("Failed to play filtered: {}", e),
                _ => eprintln!("Unexpected response from daemon"),
            }
        }
    });

    ui.on_stop_filtered(|| {
        if let Ok(ref mut c) = DaemonClient::connect() {
            match c.send_command(DaemonCommand::StopFiltered) {
                Ok(DaemonResponse::Ok) => {}
                Ok(DaemonResponse::Error(e)) => eprintln!("Failed to stop filtered: {}", e),
                _ => eprintln!("Unexpected response from daemon"),
            }
        }
    });

    ui.on_poll_playback_status({
        let ui_weak = ui_weak.clone();
        move || {
            if let Some(ui) = ui_weak.upgrade()
                && let Ok(ref mut c) = DaemonClient::connect()
                && let Ok(DaemonResponse::State(state)) = c.send_command(DaemonCommand::GetState)
            {
                ui.set_is_playing_original(state.is_playing_original);
                ui.set_is_playing_filtered(state.is_playing_filtered);
            }
        }
    });

    ui.run()?;

    Ok(())
}
