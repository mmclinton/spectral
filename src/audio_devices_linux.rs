use pipewire as pw;
use pw::{context::ContextRc, main_loop::MainLoopRc, types::ObjectType};
use std::error::Error;
use std::sync::{Arc, Mutex};

pub fn get_input_devices() -> Result<Vec<String>, Box<dyn Error>> {
    let mainloop = MainLoopRc::new(None)?;
    let context = ContextRc::new(&mainloop, None)?;
    let core = context.connect(None)?;
    let registry = core.get_registry()?;

    let devices = Arc::new(Mutex::new(Vec::<String>::new()));
    let devices_clone = devices.clone();

    let _reg_listener = registry
        .add_listener_local()
        .global(move |global| {
            if global.type_ == ObjectType::Node {
                if let Some(props) = &global.props {
                    if props.get("media.class") == Some("Audio/Source") {
                        let name = props
                            .get("node.nick")
                            .or_else(|| props.get("node.description"))
                            .or_else(|| props.get("node.name"))
                            .unwrap_or("Unknown device")
                            .to_string();

                        if !name.to_lowercase().contains("monitor") {
                            devices_clone.lock().unwrap().push(name);
                        }
                    }
                }
            }
        })
        .register();

    let loop_clone = mainloop.clone();
    let _core_listener = core
        .add_listener_local()
        .done(move |id, _seq| {
            if id == pw::core::PW_ID_CORE {
                loop_clone.quit();
            }
        })
        .register();

    core.sync(0)?;

    mainloop.run();

    let mut names = devices.lock().unwrap().clone();

    names.sort_unstable();

    if names.is_empty() {
        names = vec!["No input devices detected".to_string()];
    }

    Ok(names)
}
