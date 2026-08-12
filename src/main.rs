//! The Slint GUI control panel — a thin client over the daemon socket. It
//! does no audio work: controls send commands, and an always-on poll
//! reconciles the window with the daemon's state. Headless subcommands are
//! dispatched to [`cli`] before any UI comes up.

mod cli;
mod daemon_launch;
mod diagnostics;
mod fonts;

use spectral_lib::platform::audio_devices;
use spectral_lib::platform::ipc::{DaemonClient, DaemonCommand, DaemonResponse, DaemonState};
use spectral_lib::platform::persistence;
use spectral_lib::state::{AudioDevice, Status, is_valid_mic_name};

use slint::Model;
use slint::ModelRc;
use slint::SharedString;
use slint::VecModel;
use slint::Weak;

use std::error::Error;
use std::process::{Command, Stdio};
use std::rc::Rc;

slint::include_modules!();

/// [`is_valid_mic_name`] plus one relaxation: the field must be clearable
/// mid-edit, so empty input passes here. Enabling on an empty name is
/// refused by `on_enable_clicked`, which never sends it to the daemon.
fn is_valid_mic_name_input(text: &str) -> bool {
    text.is_empty() || is_valid_mic_name(text)
}

fn filter_devices(devices: &[AudioDevice], mic_name: &str) -> Vec<AudioDevice> {
    devices
        .iter()
        .filter(|device| device.name != mic_name)
        .cloned()
        .collect()
}

/// The row model for the source panel. An empty slice yields an empty model —
/// the panel renders its own empty state, never a placeholder row. The meta
/// line stays empty until per-device props are proven reliable.
fn device_list_model(devices: &[AudioDevice]) -> ModelRc<InputDevice> {
    let rows: Vec<InputDevice> = devices
        .iter()
        .map(|device| InputDevice {
            name: device.name.clone().into(),
            meta: SharedString::new(),
        })
        .collect();
    ModelRc::new(Rc::new(VecModel::from(rows)))
}

fn update_ui_status(ui_weak: &Weak<MainWindow>, status: Status) {
    if let Some(ui) = ui_weak.upgrade() {
        // Pending is GUI-local (staged edits over a daemon-Active mic), so
        // the pill keeps saying Active; the footer chips carry the drift.
        // The pill color follows mic_enabled in the .slint layer.
        let (status_text, mic_enabled) = match status {
            Status::Active | Status::Pending => ("Active", true),
            Status::Inactive => ("Inactive", false),
        };
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
/// Returns whether the daemon acknowledged, so callers can roll back UI
/// state that was set optimistically before the command went out.
fn send_simple_command(ui_weak: &Weak<MainWindow>, command: DaemonCommand, action: &str) -> bool {
    match DaemonClient::connect() {
        Ok(mut c) => match c.send_command(command) {
            Ok(DaemonResponse::Ok) => {
                clear_error(ui_weak);
                true
            }
            Ok(DaemonResponse::Error(e)) => {
                eprintln!("Failed to {action}: {e}");
                show_error(ui_weak, &e);
                false
            }
            Ok(DaemonResponse::State(_)) => {
                eprintln!("Unexpected response from daemon");
                show_error(ui_weak, "Unexpected response from daemon");
                false
            }
            Err(e) => {
                eprintln!("Failed to {action}: {e}");
                show_error(ui_weak, DAEMON_UNREACHABLE);
                false
            }
        },
        Err(e) => {
            eprintln!("Failed to connect to daemon: {e}");
            show_error(ui_weak, DAEMON_UNREACHABLE);
            false
        }
    }
}

/// The stop commands an exiting window still owes the daemon. The daemon owns
/// the recorder and the players and has no way to notice this window is gone —
/// the socket is opened per command, so a closed GUI looks exactly like an idle
/// one. Without this, a clip plays on to its end and a capture runs to the
/// recorder's duration cap after the window has already closed.
fn exit_stop_commands(
    is_recording: bool,
    is_playing_original: bool,
    is_playing_filtered: bool,
) -> Vec<DaemonCommand> {
    let mut commands = Vec::new();
    if is_recording {
        commands.push(DaemonCommand::StopRecording);
    }
    if is_playing_original {
        commands.push(DaemonCommand::StopOriginal);
    }
    if is_playing_filtered {
        commands.push(DaemonCommand::StopFiltered);
    }
    commands
}

/// Tear down whatever capture or playback the window still believes is
/// running. Diagnostics go to stderr rather than the error line
/// `send_simple_command` writes to, because the window this runs for is already
/// gone. Best effort by design: a killed process cannot send anything, which is
/// what the recorder's own duration cap remains the backstop for.
fn stop_playback_and_recording_on_exit(ui: &MainWindow) {
    let commands = exit_stop_commands(
        ui.get_is_recording(),
        ui.get_is_playing_original(),
        ui.get_is_playing_filtered(),
    );
    if commands.is_empty() {
        return;
    }

    let mut client = match DaemonClient::connect() {
        Ok(client) => client,
        Err(e) => {
            eprintln!("warning: cannot reach the daemon to stop the voice test: {e}");
            return;
        }
    };

    for command in commands {
        match client.send_command(command) {
            Ok(DaemonResponse::Ok) => {}
            Ok(DaemonResponse::Error(e)) => {
                eprintln!("warning: daemon refused a voice-test stop on exit: {e}");
            }
            Ok(DaemonResponse::State(_)) => {
                eprintln!("warning: unexpected response from daemon");
            }
            Err(e) => eprintln!("warning: failed to stop the voice test on exit: {e}"),
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
    // SharedString: the getter is a refcount clone, and this snapshot is
    // taken on every keystroke and threshold-drag step.
    mic_name: SharedString,
    rumble_filter: bool,
    auto_level: bool,
}

impl StagedSettings {
    fn read_from(ui: &MainWindow) -> Self {
        Self {
            device_index: ui.get_selected_input_index(),
            threshold: ui.get_slider_value(),
            mic_name: ui.get_mic_name_text(),
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
    if staged.mic_name.as_str() != applied.mic_name.borrow().as_str() {
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
    // A threshold drag re-enters here on every integer step; republishing an
    // identical list would rebuild the footer's chip row mid-drag for nothing.
    let current = ui.get_unsaved_changes();
    let unchanged = current.row_count() == changes.len()
        && current
            .iter()
            .zip(&changes)
            .all(|(row, label)| row.as_str() == *label);
    if !unchanged {
        let labels: Vec<SharedString> = changes.into_iter().map(SharedString::from).collect();
        ui.set_unsaved_changes(ModelRc::new(Rc::new(VecModel::from(labels))));
    }
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

/// The Active↔Pending drift for one staged-settings evaluation: an applied
/// (Active) mic drifts to Pending when settings diverge, and back to Active
/// when they realign. `None` means the status stays put.
fn pending_transition(current: Status, all_match: bool) -> Option<Status> {
    match current {
        Status::Active if !all_match => Some(Status::Pending),
        Status::Pending if all_match => Some(Status::Active),
        _ => None,
    }
}

/// Apply [`pending_transition`] to the shared status cell and the window.
fn apply_pending_transition(
    current_status: &std::cell::Cell<Status>,
    ui_weak: &Weak<MainWindow>,
    all_match: bool,
) {
    if let Some(next) = pending_transition(current_status.get(), all_match) {
        current_status.set(next);
        update_ui_status(ui_weak, next);
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
            ExitCode::from(cli::EXIT_USAGE_ERROR)
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
                    protocol_version: spectral_lib::platform::ipc::PROTOCOL_VERSION,
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

    // Same keep-the-window-open rationale as the daemon fallback above: an
    // empty list renders the source panel's own empty state, whose Retry
    // button re-enumerates, so the failure is recoverable in-window.
    let (input_devices, device_error) = match audio_devices::get_input_devices() {
        Ok(devices) => (devices, None),
        Err(e) => {
            eprintln!("Cannot enumerate input devices: {e}");
            (
                Vec::new(),
                Some(format!("Failed to list input devices: {e}")),
            )
        }
    };
    let filtered_devices = Rc::new(std::cell::RefCell::new(filter_devices(
        &input_devices,
        &daemon_state.mic_name,
    )));

    ui.set_input_devices(device_list_model(&filtered_devices.borrow()));

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
    // The poll's cache of the daemon's sticky last_error (display-on-change
    // only). Shared with the enable/disable handlers: those are the two
    // commands after which the daemon itself resets last_error to None, so
    // resetting the cache there re-arms the display for a failure that
    // recurs with the identical message within one poll tick.
    let last_daemon_error = Rc::new(std::cell::RefCell::new(None::<String>));
    // The newest snapshot the poll has seen, for the diagnostics dump. A
    // launch failure fabricates a defaults-only DaemonState upstream to keep
    // the window open; seeding the cache with that would report invented
    // settings as the daemon's, so it starts empty in exactly that case.
    let last_daemon_state = Rc::new(std::cell::RefCell::new(
        launch_error.is_none().then(|| daemon_state.clone()),
    ));

    refresh_unsaved_changes(&ui, &filtered_devices.borrow(), &applied);
    update_ui_status(&ui_weak, daemon_state.status);
    // A daemon-launch failure outranks the device one: it implies the
    // bigger problem, and its message names the remedy.
    if let Some(msg) = device_error {
        show_error(&ui_weak, &msg);
    }
    if let Some(msg) = launch_error {
        show_error(&ui_weak, &msg);
    }

    ui.on_selected_input_index_changed({
        let ui_weak = ui_weak.clone();
        let devices = filtered_devices.clone();
        let applied = applied.clone();
        let current_status = current_status.clone();
        move |_idx| {
            if let Some(ui) = ui_weak.upgrade() {
                let all_match = refresh_unsaved_changes(&ui, &devices.borrow(), &applied);
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
        let last_daemon_error = last_daemon_error.clone();
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

                // An unchanged Apply would still tear down and recreate the
                // mic node daemon-side, interrupting whoever captures from
                // it — skip the round trip when the daemon already runs
                // exactly these settings. The guard must share
                // refresh_unsaved_changes with the footer so "no-op" and
                // "No unsaved changes" can never disagree. Inactive falls
                // through: matching settings still need the mic started
                // (and Disable→Enable remains the restart path).
                if current_status.get() == Status::Active
                    && refresh_unsaved_changes(&ui, &devices, &applied)
                {
                    clear_error(&ui_weak);
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
                                    *last_daemon_error.borrow_mut() = None;
                                    current_status.set(Status::Active);
                                    update_ui_status(&ui_weak, Status::Active);
                                    refresh_unsaved_changes(&ui, &devices, &applied);
                                    clear_error(&ui_weak);
                                }
                                Ok(DaemonResponse::Error(e)) => {
                                    eprintln!("Failed to enable: {e}");
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
                                    eprintln!("Failed to enable: {e}");
                                    current_status.set(Status::Inactive);
                                    update_ui_status(&ui_weak, Status::Inactive);
                                    show_error(&ui_weak, DAEMON_UNREACHABLE);
                                }
                            }
                        }
                        Err(e) => {
                            eprintln!("Failed to connect to daemon: {e}");
                            show_error(&ui_weak, DAEMON_UNREACHABLE);
                        }
                    }
                } else {
                    eprintln!("Cannot enable: no input source selected");
                    show_error(&ui_weak, "Cannot enable: no input source is selected");
                }
            }
        }
    });

    ui.on_disable_clicked({
        let ui_weak = ui_weak.clone();
        let current_status = current_status.clone();
        let last_daemon_error = last_daemon_error.clone();
        move || {
            match DaemonClient::connect() {
                Ok(mut c) => match c.send_command(DaemonCommand::Disable) {
                    Ok(DaemonResponse::Ok) => {
                        *last_daemon_error.borrow_mut() = None;
                        current_status.set(Status::Inactive);
                        update_ui_status(&ui_weak, Status::Inactive);
                        clear_error(&ui_weak);
                    }
                    Ok(DaemonResponse::Error(e)) => {
                        // The daemon stops the pipeline even when source
                        // teardown fails, so Inactive is still the truth.
                        eprintln!("Failed to disable cleanly: {e}");
                        current_status.set(Status::Inactive);
                        update_ui_status(&ui_weak, Status::Inactive);
                        show_error(&ui_weak, &e);
                    }
                    Ok(DaemonResponse::State(_)) => {
                        show_error(&ui_weak, "Unexpected response from daemon");
                    }
                    Err(e) => {
                        eprintln!("Failed to disable: {e}");
                        show_error(&ui_weak, DAEMON_UNREACHABLE);
                    }
                },
                Err(e) => {
                    eprintln!("Failed to connect to daemon: {e}");
                    show_error(&ui_weak, DAEMON_UNREACHABLE);
                }
            }
        }
    });

    ui.on_mic_name_text_changed({
        let ui_weak = ui_weak.clone();
        let devices = filtered_devices.clone();
        let applied = applied.clone();
        let current_status = current_status.clone();
        let last_valid = last_valid_mic_name.clone();
        move |text| {
            if is_valid_mic_name_input(&text) {
                *last_valid.borrow_mut() = text.to_string();
                // The text field is the staged name — Enable sends it as
                // typed — so every keystroke must update the chips and the
                // Pending drift like any other control edit.
                if let Some(ui) = ui_weak.upgrade() {
                    let all_match = refresh_unsaved_changes(&ui, &devices.borrow(), &applied);
                    apply_pending_transition(&current_status, &ui_weak, all_match);
                }
                return;
            }
            let revert_to = last_valid.borrow().clone();
            let ui_weak = ui_weak.clone();
            slint::invoke_from_event_loop(move || {
                if let Some(ui) = ui_weak.upgrade() {
                    ui.set_mic_name_text(revert_to.into());
                    ui.set_mic_name_invalid_flash(true);
                }
            })
            .ok();
        }
    });

    ui.on_refresh_devices_clicked({
        let ui_weak = ui_weak.clone();
        let devices = filtered_devices.clone();
        let applied = applied.clone();
        let current_status = current_status.clone();
        move || {
            if let Some(ui) = ui_weak.upgrade() {
                match audio_devices::get_input_devices() {
                    Ok(fresh_devices) => {
                        // The poll keeps applied.mic_name in step with the
                        // daemon, so there is no fresher name a GetState
                        // round-trip could fetch. The filter itself is
                        // belt-and-braces: the virtual mic's media.class is
                        // Audio/Source/Virtual, which enumeration already
                        // excludes.
                        let mic_name = applied.mic_name.borrow().clone();
                        let filtered = filter_devices(&fresh_devices, &mic_name);

                        ui.set_input_devices(device_list_model(&filtered));

                        let new_idx = reresolve_selection(
                            &devices.borrow(),
                            ui.get_selected_input_index(),
                            &filtered,
                        );
                        ui.set_selected_input_index(new_idx);

                        *devices.borrow_mut() = filtered;
                        // The refreshed list can move or drop the applied
                        // device, and a Rust-side selection set fires no
                        // change callback — recompute the diff here or the
                        // chips and the Pending drift go stale.
                        let all_match = refresh_unsaved_changes(&ui, &devices.borrow(), &applied);
                        apply_pending_transition(&current_status, &ui_weak, all_match);
                        clear_error(&ui_weak);
                    }
                    Err(e) => {
                        eprintln!("Failed to refresh devices: {e}");
                        show_error(&ui_weak, &format!("Failed to refresh devices: {e}"));
                    }
                }
            }
        }
    });

    ui.on_open_url(|url| {
        let url_str = url.to_string();
        match Command::new("xdg-open")
            .arg(&url_str)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            // A dropped Child is never reaped and sits as a zombie until
            // the GUI exits; wait for the short-lived xdg-open off-thread.
            Ok(mut child) => {
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
            }
            Err(e) => eprintln!("Failed to open URL '{url_str}': {e}"),
        }
    });

    // The voice-test card flips its is_recording/is_playing properties before
    // these callbacks run, so a refused or undeliverable command must roll
    // the optimistic state back; the stop commands have nothing to roll back
    // (their flags are already false, and the poll reconciles the daemon).
    ui.on_start_recording({
        let ui_weak = ui_weak.clone();
        move || {
            if !send_simple_command(&ui_weak, DaemonCommand::StartRecording, "start recording")
                && let Some(ui) = ui_weak.upgrade()
            {
                ui.set_is_recording(false);
            }
        }
    });

    ui.on_stop_recording({
        let ui_weak = ui_weak.clone();
        move || {
            send_simple_command(&ui_weak, DaemonCommand::StopRecording, "stop recording");
        }
    });

    ui.on_play_original({
        let ui_weak = ui_weak.clone();
        move || {
            if !send_simple_command(&ui_weak, DaemonCommand::PlayOriginal, "play original")
                && let Some(ui) = ui_weak.upgrade()
            {
                ui.set_is_playing_original(false);
            }
        }
    });

    ui.on_stop_original({
        let ui_weak = ui_weak.clone();
        move || {
            send_simple_command(&ui_weak, DaemonCommand::StopOriginal, "stop original");
        }
    });

    ui.on_play_filtered({
        let ui_weak = ui_weak.clone();
        move || {
            if !send_simple_command(&ui_weak, DaemonCommand::PlayFiltered, "play filtered")
                && let Some(ui) = ui_weak.upgrade()
            {
                ui.set_is_playing_filtered(false);
            }
        }
    });

    ui.on_stop_filtered({
        let ui_weak = ui_weak.clone();
        move || {
            send_simple_command(&ui_weak, DaemonCommand::StopFiltered, "stop filtered");
        }
    });

    ui.on_poll_daemon_state({
        let ui_weak = ui_weak.clone();
        let devices = filtered_devices.clone();
        let applied = applied.clone();
        let current_status = current_status.clone();
        let last_daemon_error = last_daemon_error.clone();
        let last_daemon_state = last_daemon_state.clone();
        move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            match DaemonClient::connect() {
                Ok(mut c) => match c.send_command(DaemonCommand::GetState) {
                    Ok(DaemonResponse::State(state)) => {
                        // Only the success arm writes the cache: a failed poll
                        // must leave the last known good snapshot standing,
                        // since that is the one worth reporting.
                        *last_daemon_state.borrow_mut() = Some(state.clone());

                        ui.set_is_playing_original(state.is_playing_original);
                        ui.set_is_playing_filtered(state.is_playing_filtered);

                        // Another client (the CLI) can change settings while
                        // this window is open; the applied cache must track
                        // daemon truth or the chips diff against stale
                        // values and Apply silently reverts the other
                        // client's change. Staged UI properties are never
                        // written from here — the chips carry the drift.
                        applied.device_id.set(state.device_id);
                        applied.threshold.set(state.threshold);
                        applied.rumble_filter.set(state.rumble_filter);
                        applied.auto_level.set(state.auto_level);
                        if *applied.mic_name.borrow() != state.mic_name {
                            *applied.mic_name.borrow_mut() = state.mic_name.clone();
                        }

                        let display = reconcile_status(state.status, current_status.get());
                        if display != current_status.get() {
                            current_status.set(display);
                            update_ui_status(&ui_weak, display);
                        }

                        let all_match = refresh_unsaved_changes(&ui, &devices.borrow(), &applied);
                        apply_pending_transition(&current_status, &ui_weak, all_match);

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
                        // An unreachable daemon is not playing anything; a
                        // stale true here would also pin the poll timer at
                        // its 250 ms playback interval.
                        ui.set_is_playing_original(false);
                        ui.set_is_playing_filtered(false);
                    }
                },
                Err(_) => {
                    // Daemon gone means the pipeline is gone too: force the
                    // display to Inactive so the controls offer Enable. If the
                    // daemon somehow returns on its own the message lingers —
                    // accepted, nothing auto-restarts it.
                    show_error(&ui_weak, DAEMON_UNREACHABLE);
                    ui.set_is_playing_original(false);
                    ui.set_is_playing_filtered(false);
                    if current_status.get() != Status::Inactive {
                        current_status.set(Status::Inactive);
                        update_ui_status(&ui_weak, Status::Inactive);
                    }
                }
            }
        }
    });

    ui.on_error_chip_clicked({
        let ui_weak = ui_weak.clone();
        let devices = filtered_devices.clone();
        let last_daemon_state = last_daemon_state.clone();
        move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            // Snapshotted per open rather than bound: the dump is what was
            // true when the user went looking, and reopening refreshes it.
            ui.set_diagnostics_text(
                diagnostics::build_diagnostics(
                    last_daemon_state.borrow().as_ref(),
                    &devices.borrow(),
                    &daemon_launch::daemon_log_path(),
                    &persistence::get_config_path(),
                    std::env::var_os("FLATPAK_ID").is_some(),
                )
                .into(),
            );
            ui.set_show_error_popup(true);
        }
    });

    // Cleanup runs even when the loop errors out: either way the window is
    // gone and the daemon must not be left holding the mic or the speakers.
    let run_result = ui.run();
    stop_playback_and_recording_on_exit(&ui);
    run_result?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use spectral_lib::state::MIC_NAME_MAX_LEN;

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
            mic_name: mic_name.into(),
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
    fn the_name_field_accepts_empty_mid_edit_and_otherwise_defers_to_the_rule() {
        assert!(is_valid_mic_name_input(""));

        let long = "a".repeat(MIC_NAME_MAX_LEN + 1);
        for input in ["Spectral-Mic", "a_b-C9", "has space", "café", "mic🎙", &long] {
            assert_eq!(
                is_valid_mic_name_input(input),
                is_valid_mic_name(input),
                "input {input:?}"
            );
        }
    }

    #[test]
    fn device_model_maps_names_and_never_injects_placeholder_rows() {
        use slint::Model;
        assert_eq!(device_list_model(&[]).row_count(), 0);

        let model = device_list_model(&[device(10, "A"), device(20, "B")]);
        assert_eq!(model.row_count(), 2);
        assert_eq!(model.row_data(0).unwrap().name, "A");
        assert_eq!(model.row_data(1).unwrap().name, "B");
        assert!(model.row_data(0).unwrap().meta.is_empty());
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
    fn poll_reconcile_then_pending_transition_reaches_a_fixed_point_in_one_tick() {
        // The poll runs reconcile_status then pending_transition every tick;
        // if one tick's output were not the next tick's fixed point (given
        // unchanged daemon state and staged diff), the pill would oscillate.
        use Status::*;
        for daemon in [Active, Inactive, Pending] {
            for local in [Active, Inactive, Pending] {
                for all_match in [false, true] {
                    let tick = |local: Status| {
                        let s = reconcile_status(daemon, local);
                        pending_transition(s, all_match).unwrap_or(s)
                    };
                    let once = tick(local);
                    assert_eq!(
                        tick(once),
                        once,
                        "daemon {daemon:?}, local {local:?}, all_match {all_match}"
                    );
                }
            }
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

    #[test]
    fn an_idle_window_owes_the_daemon_nothing_on_exit() {
        assert!(exit_stop_commands(false, false, false).is_empty());
    }

    #[test]
    fn a_capture_in_flight_is_stopped_on_exit() {
        assert!(matches!(
            exit_stop_commands(true, false, false).as_slice(),
            [DaemonCommand::StopRecording]
        ));
    }

    #[test]
    fn each_playback_kind_is_stopped_by_its_own_command() {
        assert!(matches!(
            exit_stop_commands(false, true, false).as_slice(),
            [DaemonCommand::StopOriginal]
        ));
        assert!(matches!(
            exit_stop_commands(false, false, true).as_slice(),
            [DaemonCommand::StopFiltered]
        ));
    }

    #[test]
    fn every_active_subsystem_is_stopped_on_exit() {
        assert!(matches!(
            exit_stop_commands(true, true, true).as_slice(),
            [
                DaemonCommand::StopRecording,
                DaemonCommand::StopOriginal,
                DaemonCommand::StopFiltered
            ]
        ));
    }
}
