use crate::state::AudioDevice;
use pipewire as pw;
use pw::core::PW_ID_CORE;
use pw::spa::utils::result::AsyncSeq;
use pw::{context::ContextRc, main_loop::MainLoopRc, types::ObjectType};
use std::error::Error;
use std::process::Command;
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

                // TODO: Validate that "audio.channels" is reliably populated by PipeWire
                // for all device types. Some devices may not expose this property until
                // a stream is negotiated. Fallback to 2 (stereo) if not available.
                let channels = props
                    .get("audio.channels")
                    .and_then(|s| s.parse::<u32>().ok())
                    .unwrap_or(2);

                if !nick.to_lowercase().contains("monitor") {
                    devices_clone.lock().unwrap().push(AudioDevice {
                        id: global.id,
                        name: nick,
                        channels,
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

/// Get the channel count for a specific device by ID.
/// Returns the channel count if found, or a default of 2 (stereo) if not found.
///
/// TODO: Validate this fallback behavior - should we error instead of defaulting?
pub fn get_device_channels(device_id: u32) -> u32 {
    match get_input_devices() {
        Ok(devices) => devices
            .iter()
            .find(|d| d.id == device_id)
            .map(|d| d.channels)
            .unwrap_or(2),
        Err(_) => 2, // Default to stereo on error
    }
}

/// Get the PipeWire node name for a device by its ID.
///
/// This queries WirePlumber via `wpctl inspect` to get the node.name property,
/// which is needed for targeting streams to specific devices.
///
/// # Errors
/// Returns an error if:
/// - The `wpctl` command fails to execute
/// - The node ID doesn't exist
/// - The node.name property cannot be found in the output
pub fn get_node_name(node_id: u32) -> Result<String, Box<dyn Error>> {
    let output = Command::new("wpctl")
        .args(["inspect", &node_id.to_string()])
        .output()?;

    if !output.status.success() {
        return Err(format!("Failed to inspect node {}", node_id).into());
    }

    let output_str = String::from_utf8_lossy(&output.stdout);

    for line in output_str.lines() {
        if line.contains("node.name")
            && let Some(name_part) = line.split('"').nth(1)
        {
            return Ok(name_part.to_string());
        }
    }

    Err(format!("Could not find node.name for node {}", node_id).into())
}
