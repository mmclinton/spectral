use crate::state::AudioDevice;
use pipewire as pw;
use pw::core::PW_ID_CORE;
use pw::spa::utils::result::AsyncSeq;
use pw::{context::ContextRc, main_loop::MainLoopRc, types::ObjectType};
use std::error::Error;
use std::sync::{Arc, Mutex};

pub fn get_input_devices() -> Result<Vec<AudioDevice>, Box<dyn Error>> {
    let mainloop = MainLoopRc::new(None)?;
    let context = ContextRc::new(&mainloop, None)?;
    let core = context.connect(None)?;
    let registry = core.get_registry()?;

    let devices = Arc::new(Mutex::new(Vec::<AudioDevice>::new()));
    let devices_clone = devices.clone();

    let _reg_listener = registry
        .add_listener_local()
        .global(move |global| {
            if global.type_ == ObjectType::Node
                && let Some(props) = &global.props
                && props.get("media.class") == Some("Audio/Source")
            {
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
                    devices_clone.lock().unwrap().push(AudioDevice {
                        id: global.id,
                        name: nick,
                    });
                }
            }
        })
        .register();

    let pending = Arc::new(Mutex::new(None::<AsyncSeq>));
    let loop_clone = mainloop.clone();
    let pending_clone = pending.clone();

    let _core_listener = core
        .add_listener_local()
        .done(move |id, seq| {
            if id == PW_ID_CORE {
                let mut p = pending_clone.lock().unwrap();
                if let Some(val) = *p
                    && val == seq
                {
                    *p = None;
                    loop_clone.quit();
                }
            }
        })
        .register();

    let seq = core.sync(0)?;
    *pending.lock().unwrap() = Some(seq);

    mainloop.run();

    let input_devices = devices.lock().unwrap().clone();
    Ok(input_devices)
}

pub fn find_node_by_name(target_name: &str) -> Result<Option<u32>, Box<dyn Error>> {
    let mainloop = MainLoopRc::new(None)?;
    let context = ContextRc::new(&mainloop, None)?;
    let core = context.connect(None)?;
    let registry = core.get_registry()?;

    let found_id = Arc::new(Mutex::new(None::<u32>));
    let found_id_clone = found_id.clone();
    let target_name = target_name.to_string();

    let _reg_listener = registry
        .add_listener_local()
        .global(move |global| {
            if global.type_ == ObjectType::Node
                && let Some(props) = &global.props
            {
                // Check node.name first as it's the most precise identifier
                if let Some(name) = props.get("node.name") {
                    if name == target_name {
                        *found_id_clone.lock().unwrap() = Some(global.id);
                        return;
                    }
                }

                // Fallback: check if description matches (sometimes used as display name)
                if let Some(desc) = props.get("node.description") {
                    if desc == target_name {
                        *found_id_clone.lock().unwrap() = Some(global.id);
                        return;
                    }
                }
            }
        })
        .register();

    let pending = Arc::new(Mutex::new(None::<AsyncSeq>));
    let loop_clone = mainloop.clone();
    let pending_clone = pending.clone();

    let _core_listener = core
        .add_listener_local()
        .done(move |id, seq| {
            if id == PW_ID_CORE {
                let mut p = pending_clone.lock().unwrap();
                if let Some(val) = *p
                    && val == seq
                {
                    *p = None;
                    loop_clone.quit();
                }
            }
        })
        .register();

    let seq = core.sync(0)?;
    *pending.lock().unwrap() = Some(seq);

    mainloop.run();

    let id = *found_id.lock().unwrap();
    Ok(id)
}
