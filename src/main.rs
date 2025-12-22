mod audio_devices_linux;
mod audio_processor_linux;
mod pipewire_filter;
mod state;

use slint::ModelRc;
use slint::StandardListViewItem;
use slint::VecModel;
use slint::Weak;

use state::{AppState, Status};
use std::error::Error;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::Mutex;

slint::include_modules!();

fn main() -> Result<(), Box<dyn Error>> {
    let ui = MainWindow::new()?;
    let state = Arc::new(AppState::new());
    let ui_weak = ui.as_weak();

    let virtual_mic: Arc<Mutex<Option<audio_processor_linux::VirtualMic>>> =
        Arc::new(Mutex::new(None));

    let input_devices = audio_devices_linux::get_input_devices()?;

    let mut input_devices_vec: Vec<StandardListViewItem> = input_devices
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

    fn hex_to_color(hex: &str) -> slint::Color {
        let hex = hex.trim_start_matches('#');
        assert_eq!(hex.len(), 6, "Invalid hex color length");

        let r = u8::from_str_radix(&hex[0..2], 16).expect("Invalid red component");
        let g = u8::from_str_radix(&hex[2..4], 16).expect("Invalid green component");
        let b = u8::from_str_radix(&hex[4..6], 16).expect("Invalid blue component");

        slint::Color::from_rgb_u8(r, g, b)
    }

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

    ui.on_selected_input_index_change({
        let ui_weak = ui_weak.clone();
        let state = state.clone();
        let devices = input_devices.clone();
        move |idx: i32| {
            // (device selection needs to be applied immediately)
            let mut selected_idx = state.selected_input.lock().unwrap();
            let new_selection = if idx < 0 { None } else { Some(idx as usize) };
            if *selected_idx != new_selection {
                *selected_idx = new_selection;
                drop(selected_idx);

                let mut device_id = state.selected_device_id.lock().unwrap();
                *device_id = new_selection.and_then(|i| devices.get(i).map(|d| d.id));
                drop(device_id);

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
            let threshold_val = state.threshold_val.lock().unwrap();
            if *threshold_val != val {
                drop(threshold_val);

                let current_status = state.status.lock().unwrap().clone();
                if let Status::Active = current_status {
                    state.update_status(Status::Pending);
                    update_ui_status(&ui_weak, &state);
                }
            }
        }
    });

    ui.on_rename_clicked({
        let state = state.clone();
        let ui_weak = ui_weak.clone();

        move |new_name| {
            let new_name_str = new_name.to_string();
            let mut mic_name_guard = state.mic_name.lock().unwrap();

            if *mic_name_guard != new_name_str {
                *mic_name_guard = new_name_str;
                drop(mic_name_guard);

                let current_status = state.status.lock().unwrap().clone();
                if let Status::Active = current_status {
                    state.update_status(Status::Pending);
                }
                update_ui_status(&ui_weak, &state);
            }
        }
    });

    ui.on_reset_clicked({
        let state = state.clone();
        let ui_weak = ui_weak.clone();

        move |default_name| {
            let default_str = default_name.to_string();
            let mut mic_name_guard = state.mic_name.lock().unwrap();

            if *mic_name_guard != default_str {
                *mic_name_guard = default_str;
                drop(mic_name_guard);

                let current_status = state.status.lock().unwrap().clone();
                if let Status::Active = current_status {
                    state.update_status(Status::Pending);
                }
                update_ui_status(&ui_weak, &state);
            }
        }
    });

    ui.on_enable_clicked({
        let state = state.clone();
        let ui_weak = ui_weak.clone();
        let virtual_mic = virtual_mic.clone();

        move || {
            if let Some(ui) = ui_weak.upgrade() {
                let ui_threshold = ui.get_slider_value();
                let mut threshold_val = state.threshold_val.lock().unwrap();
                *threshold_val = ui_threshold;
            }

            let device_id = state.selected_device_id.lock().unwrap();
            let mic_name = state.mic_name.lock().unwrap().clone();

            if let Some(id) = *device_id {
                let mut mic_guard = virtual_mic.lock().unwrap();
                if let Some(mic) = mic_guard.take() {
                    mic.stop();
                }

                match audio_processor_linux::VirtualMic::create(state.clone(), id, &mic_name) {
                    Ok(mic) => {
                        println!("Virtual mic '{}' created successfully", mic_name);
                        *mic_guard = Some(mic);
                        state.update_status(Status::Active);
                    }
                    Err(e) => {
                        eprintln!("Failed to create virtual mic: {}", e);
                        state.update_status(Status::Inactive);
                    }
                }
            } else {
                eprintln!("No input device selected");
            }

            update_ui_status(&ui_weak, &state);
        }
    });

    ui.on_disable_clicked({
        let state = state.clone();
        let ui_weak = ui_weak.clone();
        let virtual_mic = virtual_mic.clone();

        move || {
            let mut mic_guard = virtual_mic.lock().unwrap();
            if let Some(mic) = mic_guard.take() {
                mic.stop();
                println!("Virtual mic stopped");
            }

            state.update_status(Status::Inactive);
            update_ui_status(&ui_weak, &state);
        }
    });

    let ui_weak_clone = ui_weak.clone();
    ui.on_mic_name_text_changed(move |text| {
        let text_str = text.to_string();
        if text_str.len() > 21 {
            if let Some(ui) = ui_weak_clone.upgrade() {
                ui.set_mic_name_text(text_str.chars().take(21).collect::<String>().into());
            }
        }
    });

    ui.on_refresh_devices_clicked({
        let ui_weak = ui_weak.clone();
        let state = state.clone();
        move || {
            if let Some(ui) = ui_weak.upgrade() {
                match audio_devices_linux::get_input_devices() {
                    Ok(fresh_devices) => {
                        let spectral_mic_name = state.mic_name.lock().unwrap().clone();

                        let filtered_devices: Vec<_> = fresh_devices
                            .iter()
                            .filter(|device| {
                                let mic_name_hyphen = spectral_mic_name.replace(" ", "-");
                                let mic_name_underscore = spectral_mic_name.replace(" ", "_");
                                let device_name = &device.name;

                                device_name != &spectral_mic_name
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

                        let selected_device_id = state.selected_device_id.lock().unwrap();
                        if let Some(device_id) = *selected_device_id {
                            let new_index = filtered_devices
                                .iter()
                                .position(|d| d.id == device_id)
                                .map(|pos| pos as i32)
                                .unwrap_or(-1);

                            ui.set_selected_input_index(new_index);

                            let mut selected_idx = state.selected_input.lock().unwrap();
                            *selected_idx = if new_index >= 0 {
                                Some(new_index as usize)
                            } else {
                                None
                            };
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
