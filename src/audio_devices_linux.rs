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
                            devices_clone.lock().unwrap().push(AudioDevice {
                                id: global.id,
                                name: nick,
                            });
                        }
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

    let input_devices = devices.lock().unwrap().clone();
    Ok(input_devices)
}
