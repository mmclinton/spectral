//! The Slint GUI control panel — a thin client over the daemon socket. It
//! does no audio work: controls send commands, and an always-on poll
//! reconciles the window with the daemon's state. Headless subcommands are
//! dispatched to [`cli`] before any UI comes up.

mod cli;
mod daemon_launch;
mod fonts;

use spectral_lib::audio_devices;
use spectral_lib::ipc::{DaemonClient, DaemonCommand, DaemonResponse, DaemonState};
use spectral_lib::state::{AudioDevice, MIC_NAME_MAX_LEN, Status};

use slint::ModelRc;
use slint::SharedString;
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
        // Pending is GUI-local (staged edits over a daemon-Active mic), so
        // the pill keeps saying Active; the footer chips carry the drift.
        let (color_str, status_text, mic_enabled) = match status {
            Status::Active | Status::Pending => ("#2EA043", "Active", true),
            Status::Inactive => ("#8B949E", "Inactive", false),
        };
        ui.set_status_color(hex_to_color(color_str));
        ui.set_status_text(status_text.into());
        ui.set_mic_enabled(mic_enabled);
    }
}

const DAEMON_UNREACHABLE: &str = "Cannot reach the daemon — restart Spectral to relaunch it";

fn show_error(ui_weak: &Weak<MainWindow>, msg: &str) {
    if let Some(ui) = ui_weak.upgrade() {
        ui.set_error_text(msg.into());
    }
}

fn clear_error(ui_weak: &Weak<MainWindow>) {
    show_error(ui_weak, "");
}

/// Send a command whose only interesting outcome is success or an error
/// message; failures land in the window's error line, successes clear it.
fn send_simple_command(ui_weak: &Weak<MainWindow>, command: DaemonCommand, action: &str) {
    match DaemonClient::connect() {
        Ok(mut c) => match c.send_command(command) {
            Ok(DaemonResponse::Ok) => clear_error(ui_weak),
            Ok(DaemonResponse::Error(e)) => {
                eprintln!("Failed to {action}: {e}");
                show_error(ui_weak, &e);
            }
            Ok(DaemonResponse::State(_)) => {
                eprintln!("Unexpected response from daemon");
                show_error(ui_weak, "Unexpected response from daemon");
            }
            Err(e) => {
                eprintln!("Failed to {action}: {e}");
                show_error(ui_weak, DAEMON_UNREACHABLE);
            }
        },
        Err(e) => {
            eprintln!("Failed to connect to daemon: {e}");
            show_error(ui_weak, DAEMON_UNREACHABLE);
        }
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

/// Where the previously selected device lands in a refreshed list: an index
/// is only meaningful within one list, so the selection carries across a
/// refresh by device id, not by position. No previous selection — or a
/// device that vanished — resolves to -1 (nothing selected).
fn reresolve_selection(old: &[AudioDevice], old_idx: i32, new: &[AudioDevice]) -> i32 {
    usize::try_from(old_idx)
        .ok()
        .and_then(|idx| old.get(idx))
        .and_then(|device| new.iter().position(|d| d.id == device.id))
        .map_or(-1, |idx| idx as i32)
}

/// The last settings the daemon acknowledged; staged UI edits are compared
/// against these to drive the Active↔Pending display.
struct AppliedSettings {
    device_id: std::cell::Cell<Option<u32>>,
    threshold: std::cell::Cell<i32>,
    mic_name: std::cell::RefCell<String>,
    rumble_filter: std::cell::Cell<bool>,
    auto_level: std::cell::Cell<bool>,
}

/// Snapshot of the staged control values, read from the UI properties, which
/// Slint updates before the change callbacks fire.
struct StagedSettings {
    device_index: i32,
    threshold: i32,
    mic_name: String,
    rumble_filter: bool,
    auto_level: bool,
}

impl StagedSettings {
    fn read_from(ui: &MainWindow) -> Self {
        Self {
            device_index: ui.get_selected_input_index(),
            threshold: ui.get_slider_value(),
            mic_name: ui.get_mic_name_text().to_string(),
            rumble_filter: ui.get_rumble_filter(),
            auto_level: ui.get_auto_level(),
        }
    }
}

/// Chip labels for every staged setting that diverges from the applied ones,
/// in the footer's canonical order. Empty means everything matches.
fn changed_settings(
    staged: &StagedSettings,
    devices: &[AudioDevice],
    applied: &AppliedSettings,
) -> Vec<&'static str> {
    let mut changed = Vec::new();
    if staged.threshold != applied.threshold.get() {
        changed.push("threshold");
    }
    if staged.rumble_filter != applied.rumble_filter.get() {
        changed.push("rumble filter");
    }
    if staged.mic_name != *applied.mic_name.borrow() {
        changed.push("mic name");
    }
    if !device_matches_applied(staged.device_index, devices, &applied.device_id) {
        changed.push("input source");
    }
    if staged.auto_level != applied.auto_level.get() {
        changed.push("auto level");
    }
    changed
}

/// Recompute the staged-vs-applied diff, publish the footer's chip list, and
/// report whether everything matches. The chips and the Pending drift both
/// come from this one evaluation so they cannot disagree.
fn refresh_unsaved_changes(
    ui: &MainWindow,
    devices: &[AudioDevice],
    applied: &AppliedSettings,
) -> bool {
    let changes = changed_settings(&StagedSettings::read_from(ui), devices, applied);
    let all_match = changes.is_empty();
    let labels: Vec<SharedString> = changes.into_iter().map(SharedString::from).collect();
    ui.set_unsaved_changes(ModelRc::new(Rc::new(VecModel::from(labels))));
    all_match
}

/// The status to display given the daemon's truth and the local view: staged
/// but unapplied edits (a local Pending over a daemon Active) survive the
/// poll; everything else defers to the daemon.
fn reconcile_status(daemon: Status, local: Status) -> Status {
    match (daemon, local) {
        (Status::Active, Status::Pending) => Status::Pending,
        (daemon, _) => daemon,
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

fn main() -> std::process::ExitCode {
    use std::process::ExitCode;

    match cli::parse(std::env::args_os().skip(1)) {
        Ok(cli::CliCommand::Gui) => {
            unsafe { std::env::set_var("SLINT_BACKEND", "winit-software") };
            match run_app() {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("Error: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Ok(command) => cli::run(command),
        Err(msg) => {
            eprintln!("spectral: {msg}\nTry 'spectral --help'.");
            ExitCode::from(2)
        }
    }
}

fn run_app() -> Result<(), Box<dyn Error>> {
    let (daemon_state, launch_error) = match daemon_launch::connect_and_fetch_state() {
        Ok((_, state)) => (state, None),
        Err(e) => {
            // Still open the window: a .desktop launch has no stderr, so a
            // silent exit looks like the app is broken. The defaults mirror
            // the .slint property defaults; Inactive keeps controls disabled.
            eprintln!("Cannot reach the daemon: {e}");
            (
                DaemonState {
                    protocol_version: spectral_lib::ipc::PROTOCOL_VERSION,
                    status: Status::Inactive,
                    device_id: None,
                    device_node_name: None,
                    mic_name: "Spectral-Mic".to_string(),
                    threshold: 50,
                    rumble_filter: false,
                    auto_level: false,
                    is_playing_original: false,
                    is_playing_filtered: false,
                    last_error: None,
                },
                Some(format!("Cannot reach the daemon: {e}")),
            )
        }
    };

    let ui = MainWindow::new()?;
    // Must come after MainWindow::new(): until the Slint platform context exists,
    // set_xdg_app_id returns Err and stores nothing, leaving the Wayland surface
    // without an app_id so the desktop file (and taskbar icon) can't be matched.
    if let Err(e) = slint::set_xdg_app_id("page.codeberg.mmc.Spectral") {
        eprintln!("warning: failed to set xdg app id (taskbar icon may be generic): {e}");
    }
    match fonts::preferred_monospace_family() {
        Some(family) => ui.set_monospace_font_family(family.into()),
        None => eprintln!("warning: no monospaced font found; columns may not align"),
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
    ui.set_rumble_filter(daemon_state.rumble_filter);
    ui.set_auto_level(daemon_state.auto_level);

    let last_valid_mic_name = Rc::new(std::cell::RefCell::new(daemon_state.mic_name.clone()));

    if let Some(device_id) = daemon_state.device_id
        && let Some(idx) = filtered_devices
            .borrow()
            .iter()
            .position(|d| d.id == device_id)
    {
        ui.set_selected_input_index(idx as i32);
    }

    let applied = Rc::new(AppliedSettings {
        device_id: std::cell::Cell::new(daemon_state.device_id),
        threshold: std::cell::Cell::new(daemon_state.threshold),
        mic_name: std::cell::RefCell::new(daemon_state.mic_name.clone()),
        rumble_filter: std::cell::Cell::new(daemon_state.rumble_filter),
        auto_level: std::cell::Cell::new(daemon_state.auto_level),
    });
    let current_status = Rc::new(std::cell::Cell::new(daemon_state.status));

    refresh_unsaved_changes(&ui, &filtered_devices.borrow(), &applied);
    update_ui_status(&ui_weak, daemon_state.status);
    if let Some(msg) = launch_error {
        show_error(&ui_weak, &msg);
    }

    ui.on_selected_input_index_changed({
        let ui_weak = ui_weak.clone();
        let devices = filtered_devices.clone();
        let applied = applied.clone();
        let current_status = current_status.clone();
        move |idx: i32| {
            let devices = devices.borrow();
            if idx >= 0
                && (idx as usize) < devices.len()
                && let Some(ui) = ui_weak.upgrade()
            {
                let all_match = refresh_unsaved_changes(&ui, &devices, &applied);
                apply_pending_transition(&current_status, &ui_weak, all_match);
            }
        }
    });

    ui.on_threshold_changed({
        let ui_weak = ui_weak.clone();
        let devices = filtered_devices.clone();
        let applied = applied.clone();
        let current_status = current_status.clone();
        move |_val| {
            if let Some(ui) = ui_weak.upgrade() {
                let all_match = refresh_unsaved_changes(&ui, &devices.borrow(), &applied);
                apply_pending_transition(&current_status, &ui_weak, all_match);
            }
        }
    });

    ui.on_rumble_filter_changed({
        let ui_weak = ui_weak.clone();
        let devices = filtered_devices.clone();
        let applied = applied.clone();
        let current_status = current_status.clone();
        move |_checked| {
            if let Some(ui) = ui_weak.upgrade() {
                let all_match = refresh_unsaved_changes(&ui, &devices.borrow(), &applied);
                apply_pending_transition(&current_status, &ui_weak, all_match);
            }
        }
    });

    ui.on_auto_level_changed({
        let ui_weak = ui_weak.clone();
        let devices = filtered_devices.clone();
        let applied = applied.clone();
        let current_status = current_status.clone();
        move |_checked| {
            if let Some(ui) = ui_weak.upgrade() {
                let all_match = refresh_unsaved_changes(&ui, &devices.borrow(), &applied);
                apply_pending_transition(&current_status, &ui_weak, all_match);
            }
        }
    });

    ui.on_rename_clicked({
        let ui_weak = ui_weak.clone();
        let devices = filtered_devices.clone();
        let applied = applied.clone();
        let current_status = current_status.clone();
        move |_new_name| {
            if let Some(ui) = ui_weak.upgrade() {
                let all_match = refresh_unsaved_changes(&ui, &devices.borrow(), &applied);
                apply_pending_transition(&current_status, &ui_weak, all_match);
            }
        }
    });

    ui.on_reset_clicked({
        let ui_weak = ui_weak.clone();
        let devices = filtered_devices.clone();
        let applied = applied.clone();
        let current_status = current_status.clone();
        let last_valid = last_valid_mic_name.clone();
        move |default_name| {
            if let Some(ui) = ui_weak.upgrade() {
                *last_valid.borrow_mut() = default_name.to_string();
                let all_match = refresh_unsaved_changes(&ui, &devices.borrow(), &applied);
                apply_pending_transition(&current_status, &ui_weak, all_match);
            }
        }
    });

    ui.on_enable_clicked({
        let ui_weak = ui_weak.clone();
        let devices = filtered_devices.clone();
        let applied = applied.clone();
        let current_status = current_status.clone();
        move || {
            let devices = devices.borrow();
            if let Some(ui) = ui_weak.upgrade() {
                let idx = ui.get_selected_input_index();
                let threshold = ui.get_slider_value();
                let rumble_filter = ui.get_rumble_filter();
                let auto_level = ui.get_auto_level();
                let mic_name = ui.get_mic_name_text().to_string();
                if mic_name.is_empty() {
                    eprintln!("Cannot enable: mic name is empty");
                    show_error(&ui_weak, "Cannot enable: the mic name is empty");
                    return;
                }

                if idx >= 0 && (idx as usize) < devices.len() {
                    match DaemonClient::connect() {
                        Ok(mut c) => {
                            let device_id = devices[idx as usize].id;
                            match c.send_command(DaemonCommand::Enable {
                                device_id: Some(device_id),
                                mic_name: Some(mic_name.clone()),
                                threshold: Some(threshold),
                                rumble_filter: Some(rumble_filter),
                                auto_level: Some(auto_level),
                            }) {
                                Ok(DaemonResponse::Ok) => {
                                    applied.threshold.set(threshold);
                                    applied.device_id.set(Some(device_id));
                                    applied.rumble_filter.set(rumble_filter);
                                    applied.auto_level.set(auto_level);
                                    *applied.mic_name.borrow_mut() = mic_name;
                                    current_status.set(Status::Active);
                                    update_ui_status(&ui_weak, Status::Active);
                                    refresh_unsaved_changes(&ui, &devices, &applied);
                                    clear_error(&ui_weak);
                                }
                                Ok(DaemonResponse::Error(e)) => {
                                    eprintln!("Failed to enable: {}", e);
                                    current_status.set(Status::Inactive);
                                    update_ui_status(&ui_weak, Status::Inactive);
                                    show_error(&ui_weak, &e);
                                }
                                Ok(DaemonResponse::State(_)) => {
                                    current_status.set(Status::Inactive);
                                    update_ui_status(&ui_weak, Status::Inactive);
                                    show_error(&ui_weak, "Unexpected response from daemon");
                                }
                                Err(e) => {
                                    eprintln!("Failed to enable: {}", e);
                                    current_status.set(Status::Inactive);
                                    update_ui_status(&ui_weak, Status::Inactive);
                                    show_error(&ui_weak, DAEMON_UNREACHABLE);
                                }
                            }
                        }
                        Err(e) => {
                            eprintln!("Failed to connect to daemon: {}", e);
                            show_error(&ui_weak, DAEMON_UNREACHABLE);
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
            match DaemonClient::connect() {
                Ok(mut c) => match c.send_command(DaemonCommand::Disable) {
                    Ok(DaemonResponse::Ok) => {
                        current_status.set(Status::Inactive);
                        update_ui_status(&ui_weak, Status::Inactive);
                        clear_error(&ui_weak);
                    }
                    Ok(DaemonResponse::Error(e)) => {
                        // The daemon stops the pipeline even when source
                        // teardown fails, so Inactive is still the truth.
                        eprintln!("Failed to disable cleanly: {}", e);
                        current_status.set(Status::Inactive);
                        update_ui_status(&ui_weak, Status::Inactive);
                        show_error(&ui_weak, &e);
                    }
                    Ok(DaemonResponse::State(_)) => {
                        show_error(&ui_weak, "Unexpected response from daemon");
                    }
                    Err(e) => {
                        eprintln!("Failed to disable: {}", e);
                        show_error(&ui_weak, DAEMON_UNREACHABLE);
                    }
                },
                Err(e) => {
                    eprintln!("Failed to connect to daemon: {}", e);
                    show_error(&ui_weak, DAEMON_UNREACHABLE);
                }
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

                        let new_idx = reresolve_selection(
                            &devices.borrow(),
                            ui.get_selected_input_index(),
                            &filtered,
                        );
                        ui.set_selected_input_index(new_idx);

                        *devices.borrow_mut() = filtered;
                        clear_error(&ui_weak);
                    }
                    Err(e) => {
                        eprintln!("Failed to refresh devices: {}", e);
                        show_error(&ui_weak, &format!("Failed to refresh devices: {e}"));
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

    ui.on_start_recording({
        let ui_weak = ui_weak.clone();
        move || send_simple_command(&ui_weak, DaemonCommand::StartRecording, "start recording")
    });

    ui.on_stop_recording({
        let ui_weak = ui_weak.clone();
        move || send_simple_command(&ui_weak, DaemonCommand::StopRecording, "stop recording")
    });

    ui.on_play_original({
        let ui_weak = ui_weak.clone();
        move || send_simple_command(&ui_weak, DaemonCommand::PlayOriginal, "play original")
    });

    ui.on_stop_original({
        let ui_weak = ui_weak.clone();
        move || send_simple_command(&ui_weak, DaemonCommand::StopOriginal, "stop original")
    });

    ui.on_play_filtered({
        let ui_weak = ui_weak.clone();
        move || send_simple_command(&ui_weak, DaemonCommand::PlayFiltered, "play filtered")
    });

    ui.on_stop_filtered({
        let ui_weak = ui_weak.clone();
        move || send_simple_command(&ui_weak, DaemonCommand::StopFiltered, "stop filtered")
    });

    ui.on_poll_daemon_state({
        let ui_weak = ui_weak.clone();
        let current_status = current_status.clone();
        let last_daemon_error = std::cell::RefCell::new(None::<String>);
        move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            match DaemonClient::connect() {
                Ok(mut c) => match c.send_command(DaemonCommand::GetState) {
                    Ok(DaemonResponse::State(state)) => {
                        ui.set_is_playing_original(state.is_playing_original);
                        ui.set_is_playing_filtered(state.is_playing_filtered);

                        let display = reconcile_status(state.status, current_status.get());
                        if display != current_status.get() {
                            current_status.set(display);
                            update_ui_status(&ui_weak, display);
                        }

                        // Show a daemon error only when it changes so a sticky
                        // one can't stomp fresher local messages every tick;
                        // None never clears (local errors must outlive a tick).
                        if state.last_error != *last_daemon_error.borrow() {
                            if let Some(msg) = &state.last_error {
                                show_error(&ui_weak, msg);
                            }
                            *last_daemon_error.borrow_mut() = state.last_error;
                        }
                    }
                    Ok(_) => show_error(&ui_weak, "Unexpected response from daemon"),
                    Err(_) => {
                        show_error(&ui_weak, DAEMON_UNREACHABLE);
                    }
                },
                Err(_) => {
                    // Daemon gone means the pipeline is gone too: force the
                    // display to Inactive so the controls offer Enable. If the
                    // daemon somehow returns on its own the message lingers —
                    // accepted, nothing auto-restarts it.
                    show_error(&ui_weak, DAEMON_UNREACHABLE);
                    if current_status.get() != Status::Inactive {
                        current_status.set(Status::Inactive);
                        update_ui_status(&ui_weak, Status::Inactive);
                    }
                }
            }
        }
    });

    ui.run()?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(id: u32, name: &str) -> AudioDevice {
        AudioDevice {
            id,
            name: name.to_string(),
            channels: 2,
        }
    }

    fn staged(
        device_index: i32,
        threshold: i32,
        mic_name: &str,
        rumble_filter: bool,
        auto_level: bool,
    ) -> StagedSettings {
        StagedSettings {
            device_index,
            threshold,
            mic_name: mic_name.to_string(),
            rumble_filter,
            auto_level,
        }
    }

    fn applied(
        device_id: Option<u32>,
        threshold: i32,
        mic_name: &str,
        rumble_filter: bool,
        auto_level: bool,
    ) -> AppliedSettings {
        AppliedSettings {
            device_id: std::cell::Cell::new(device_id),
            threshold: std::cell::Cell::new(threshold),
            mic_name: std::cell::RefCell::new(mic_name.to_string()),
            rumble_filter: std::cell::Cell::new(rumble_filter),
            auto_level: std::cell::Cell::new(auto_level),
        }
    }

    #[test]
    fn no_chips_when_staged_settings_match_applied() {
        let devices = [device(10, "A")];
        let applied = applied(Some(10), 50, "Spectral-Mic", false, false);
        let staged = staged(0, 50, "Spectral-Mic", false, false);
        assert!(changed_settings(&staged, &devices, &applied).is_empty());
    }

    #[test]
    fn each_divergent_setting_yields_its_chip_label() {
        let devices = [device(10, "A"), device(20, "B")];
        let applied = applied(Some(10), 50, "Spectral-Mic", false, false);
        let cases = [
            (staged(0, 80, "Spectral-Mic", false, false), "threshold"),
            (staged(0, 50, "Spectral-Mic", true, false), "rumble filter"),
            (staged(0, 50, "Denoised", false, false), "mic name"),
            (staged(1, 50, "Spectral-Mic", false, false), "input source"),
            (staged(0, 50, "Spectral-Mic", false, true), "auto level"),
        ];
        for (staged, expected) in cases {
            assert_eq!(changed_settings(&staged, &devices, &applied), [expected]);
        }
    }

    #[test]
    fn chip_labels_follow_the_footer_canonical_order() {
        let devices = [device(10, "A"), device(20, "B")];
        let applied = applied(Some(10), 50, "Spectral-Mic", false, false);
        let staged = staged(1, 80, "Denoised", true, true);
        assert_eq!(
            changed_settings(&staged, &devices, &applied),
            [
                "threshold",
                "rumble filter",
                "mic name",
                "input source",
                "auto level"
            ]
        );
    }

    #[test]
    fn the_input_source_chip_follows_the_device_id_not_the_index() {
        // The list was reordered since Enable: the id at the staged index is
        // what decides, not the position.
        let reordered = [device(20, "B"), device(10, "A")];
        let applied_some = applied(Some(10), 50, "Spectral-Mic", false, false);
        let applied_none = applied(None, 50, "Spectral-Mic", false, false);
        let cases = [
            (1, &applied_some, false),
            (0, &applied_some, true),
            // Out-of-range means "no device": a chip only when one is applied.
            (-1, &applied_none, false),
            (-1, &applied_some, true),
        ];
        for (idx, applied, expect_chip) in cases {
            let changes = changed_settings(
                &staged(idx, 50, "Spectral-Mic", false, false),
                &reordered,
                applied,
            );
            let expected: &[&str] = if expect_chip { &["input source"] } else { &[] };
            assert_eq!(changes, expected, "idx {idx}");
        }
    }

    #[test]
    fn refresh_carries_the_selection_by_device_id_not_by_index() {
        let dev = |id: u32, name: &str| AudioDevice {
            id,
            name: name.to_string(),
            channels: 2,
        };
        let old = [dev(10, "A"), dev(20, "B"), dev(30, "C")];
        let cases = [
            // Unchanged list: selection stays put.
            (1, vec![dev(10, "A"), dev(20, "B"), dev(30, "C")], 1),
            // Reordered list: follow the id, not the position.
            (1, vec![dev(20, "B"), dev(10, "A")], 0),
            // The selected device vanished: clear the selection.
            (2, vec![dev(10, "A"), dev(20, "B")], -1),
            // Nothing was selected: nothing to carry over.
            (-1, vec![dev(10, "A")], -1),
            // Defensive: an out-of-range index clears rather than panics.
            (9, vec![dev(10, "A")], -1),
        ];
        for (old_idx, new, expected) in cases {
            assert_eq!(
                reresolve_selection(&old, old_idx, &new),
                expected,
                "old_idx {old_idx}"
            );
        }
    }

    #[test]
    fn reconcile_prefers_daemon_truth_except_for_staged_edits() {
        use Status::*;
        let cases = [
            (Active, Active, Active),
            (Active, Pending, Pending),
            (Active, Inactive, Active),
            (Inactive, Active, Inactive),
            (Inactive, Pending, Inactive),
            (Inactive, Inactive, Inactive),
            (Pending, Active, Pending),
            (Pending, Pending, Pending),
            (Pending, Inactive, Pending),
        ];
        for (daemon, local, expected) in cases {
            assert_eq!(
                reconcile_status(daemon, local),
                expected,
                "daemon {daemon:?}, local {local:?}"
            );
        }
    }
}
