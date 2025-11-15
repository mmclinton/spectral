#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod state;
use slint::Weak;
use state::AppState;
use state::Status;
use std::error::Error;
use std::sync::Arc;

slint::include_modules!();

fn main() -> Result<(), Box<dyn Error>> {
    let ui = MainWindow::new()?;
    let state = Arc::new(AppState::new());

    let ui_weak = ui.as_weak();

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
        move |idx| {
            let mut selected_idx = state.selected_input.lock().unwrap();
            if *selected_idx != Some(idx as usize) {
                *selected_idx = Some(idx as usize);
                state.update_status(Status::Pending);
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
