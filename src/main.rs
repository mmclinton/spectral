#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod state;
use pipewire as pw;
use pw::core::PW_ID_CORE;
use pw::spa::utils::result::AsyncSeq;
use pw::{context::ContextRc, main_loop::MainLoopRc, types::ObjectType};
use slint::ModelRc;
use slint::StandardListViewItem;
use slint::VecModel;
use slint::Weak;
use state::AppState;
use state::Status;
use std::error::Error;
use std::rc::Rc;
use std::sync::Arc;

slint::include_modules!();

fn main() -> Result<(), Box<dyn Error>> {
    let ui = MainWindow::new()?;
    let state = Arc::new(AppState::new());
    let ui_weak = ui.as_weak();

    let mainloop = MainLoopRc::new(None)?;
    let context = ContextRc::new(&mainloop, None)?;
    let core = context.connect(None)?;
    let registry = core.get_registry()?;

    let devices = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let devices_clone = devices.clone();

    let _reg_listener = registry
        .add_listener_local()
        .global(move |global| {
            if global.type_ == ObjectType::Node {
                if let Some(props) = &global.props {
                    if props.get("media.class") == Some("Audio/Source") {
                        let nick = props
                            .get("node.nick")
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| {
                                props
                                    .get("node.description")
                                    .map(|s| s.to_string())
                                    .unwrap_or("Unknown".to_string())
                            });
                        if !nick.to_lowercase().contains("monitor") {
                            devices_clone.lock().unwrap().push(nick);
                        }
                    }
                }
            }
        })
        .register();

    let pending = Arc::new(std::sync::Mutex::new(None::<AsyncSeq>));
    let loop_clone = mainloop.clone();
    let pending_clone = pending.clone();

    let _core_listener = core
        .add_listener_local()
        .done(move |id, seq| {
            if id == PW_ID_CORE {
                let mut p = pending_clone.lock().unwrap();
                if let Some(val) = *p {
                    if val == seq {
                        *p = None;
                        loop_clone.quit();
                    }
                }
            }
        })
        .register();

    let seq = core.sync(0)?;
    *pending.lock().unwrap() = Some(seq);

    mainloop.run();

    let input_names = devices.lock().unwrap();
    let mut input_devices_vec: Vec<StandardListViewItem> = input_names
        .iter()
        .map(|name| {
            let mut item = StandardListViewItem::default();
            item.text = name.clone().into();
            item
        })
        .collect();

    if input_devices_vec.is_empty() {
        let mut item = StandardListViewItem::default();
        item.text = "No input devices detected".into();
        input_devices_vec.push(item);
    }

    let vec_model = VecModel::from(input_devices_vec);
    let model: ModelRc<StandardListViewItem> = ModelRc::new(Rc::new(vec_model));
    ui.set_input_devices(model);

    fn update_ui_status(ui_weak: &Weak<MainWindow>, state: &Arc<AppState>) {
        if let Some(ui) = ui_weak.upgrade() {
            let status = state.status.lock().unwrap();
            let color_str = match *status {
                Status::Inactive => "#FF0000",
                Status::Active => "#3FB950",
                Status::Pending => "#FFFF00",
            };
            ui.set_status_color(hex_to_color(color_str));
            ui.set_status_text(match *status {
                Status::Inactive => "Status: Inactive".into(),
                Status::Active => "Status: Active".into(),
                Status::Pending => "Status: Pending Changes".into(),
            });
        }
    }

    fn hex_to_color(hex: &str) -> slint::Color {
        let hex = hex.trim_start_matches('#');
        assert_eq!(hex.len(), 6, "Invalid hex color length");

        let r = u8::from_str_radix(&hex[0..2], 16).expect("Invalid red component");
        let g = u8::from_str_radix(&hex[2..4], 16).expect("Invalid green component");
        let b = u8::from_str_radix(&hex[4..6], 16).expect("Invalid blue component");

        slint::Color::from_rgb_u8(r, g, b)
    }

    ui.on_selected_input_index_change({
        let ui_weak = ui_weak.clone();
        let state = state.clone();
        move |idx: i32| {
            let mut selected_idx = state.selected_input.lock().unwrap();
            let new_selection = if idx < 0 { None } else { Some(idx as usize) };
            if *selected_idx != new_selection {
                *selected_idx = new_selection;
                let current_status = state.status.lock().unwrap().clone();
                if let Status::Active = current_status {
                    state.update_status(Status::Pending);
                }
                update_ui_status(&ui_weak, &state);
            }
        }
    });

    ui.on_threshold_change({
        let ui_weak = ui_weak.clone();
        let state = state.clone();
        move |val| {
            let mut threshold_val = state.threshold_val.lock().unwrap();
            if *threshold_val != val {
                *threshold_val = val;
                state.update_status(Status::Pending);
                update_ui_status(&ui_weak, &state);
            }
        }
    });

    ui.on_rename_clicked({
        let state = state.clone();
        let ui_weak = ui_weak.clone();
        move |new_name| {
            let mut mic_name_guard = state.mic_name.lock().unwrap();
            let new_name_str = new_name.to_string();
            if *mic_name_guard != new_name_str {
                *mic_name_guard = new_name_str;
                state.update_status(Status::Pending);
                update_ui_status(&ui_weak, &state);
            }
        }
    });

    ui.on_reset_clicked({
        let state = state.clone();
        let ui_weak = ui_weak.clone();
        move |default_name| {
            let mut mic_name_guard = state.mic_name.lock().unwrap();
            let default_str = default_name.to_string();
            if *mic_name_guard != default_str {
                *mic_name_guard = default_str;
                state.update_status(Status::Pending);
                update_ui_status(&ui_weak, &state);
            }
        }
    });

    ui.on_enable_clicked({
        let state = state.clone();
        let ui_weak = ui_weak.clone();
        move || {
            state.update_status(Status::Active);
            update_ui_status(&ui_weak, &state);
        }
    });

    ui.on_disable_clicked({
        let state = state.clone();
        let ui_weak = ui_weak.clone();
        move || {
            state.update_status(Status::Inactive);
            update_ui_status(&ui_weak, &state);
        }
    });

    ui.run()?;

    Ok(())
}
