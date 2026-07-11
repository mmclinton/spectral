//! PipeWire registry queries: enumerating input sources, resolving node
//! names, and pointing the session manager's default source at the mic.
//! Each query opens its own short-lived PipeWire connection.

use crate::state::{AudioDevice, lock_or_recover};
use pipewire as pw;
use pw::core::PW_ID_CORE;
use pw::metadata::Metadata;
use pw::properties::PropertiesBox;
use pw::registry::GlobalObject;
use pw::spa::utils::dict::DictRef;
use pw::spa::utils::result::AsyncSeq;
use pw::{context::ContextRc, main_loop::MainLoopRc, types::ObjectType};
use std::error::Error;
use std::sync::{Arc, Mutex};

/// Connect, enumerate every registry global once, and collect whatever
/// `visit` extracts. One transient PipeWire connection per call.
fn collect_globals<T, F>(visit: F) -> Result<Vec<T>, Box<dyn Error>>
where
    T: 'static,
    F: Fn(&GlobalObject<&DictRef>) -> Option<T> + 'static,
{
    let mainloop = MainLoopRc::new(None)?;
    let context = ContextRc::new(&mainloop, None)?;
    let core = context.connect(None)?;
    let registry = core.get_registry()?;

    let found = Arc::new(Mutex::new(Vec::<T>::new()));
    let found_clone = found.clone();

    let _reg_listener = registry
        .add_listener_local()
        .global(move |global| {
            if let Some(item) = visit(global) {
                lock_or_recover(&found_clone).push(item);
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
                let mut p = lock_or_recover(&pending_clone);
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
    *lock_or_recover(&pending) = Some(seq);

    mainloop.run();

    let items = std::mem::take(&mut *lock_or_recover(&found));
    Ok(items)
}

/// Enumerates `Audio/Source` nodes, skipping monitor sources; display names
/// prefer `node.nick` over the longer `node.description`.
pub fn get_input_devices() -> Result<Vec<AudioDevice>, Box<dyn Error>> {
    collect_globals(|global| {
        if global.type_ != ObjectType::Node {
            return None;
        }
        let props = global.props.as_ref()?;
        if props.get("media.class") != Some("Audio/Source") {
            return None;
        }

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

        (!nick.to_lowercase().contains("monitor")).then_some(AudioDevice {
            id: global.id,
            name: nick,
            channels,
        })
    })
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
        Err(_) => 2,
    }
}

/// Point the configured default input at `node_name` — the persisted
/// equivalent of `wpctl set-default`. WirePlumber stores the value and
/// re-applies it whenever a node with that name (re)appears.
pub fn set_default_source(node_name: &str) -> Result<(), Box<dyn Error>> {
    let mainloop = MainLoopRc::new(None)?;
    let context = ContextRc::new(&mainloop, None)?;
    let core = context.connect(None)?;
    let registry = core.get_registry()?;

    // Rc, not Arc: the proxy global is !Send and never leaves this thread.
    let metadata_global =
        std::rc::Rc::new(std::cell::RefCell::new(None::<GlobalObject<PropertiesBox>>));
    let global_clone = metadata_global.clone();

    let _reg_listener = registry
        .add_listener_local()
        .global(move |global| {
            if global.type_ == ObjectType::Metadata
                && let Some(props) = &global.props
                && props.get("metadata.name") == Some("default")
            {
                *global_clone.borrow_mut() = Some(global.to_owned());
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
                let mut p = lock_or_recover(&pending_clone);
                if let Some(val) = *p
                    && val == seq
                {
                    *p = None;
                    loop_clone.quit();
                }
            }
        })
        .register();

    // Roundtrip 1: enumerate the globals so the session manager's "default"
    // metadata object is discovered.
    *lock_or_recover(&pending) = Some(core.sync(0)?);
    mainloop.run();

    let global = metadata_global
        .borrow_mut()
        .take()
        .ok_or("no \"default\" metadata object found (is a session manager running?)")?;
    let metadata: Metadata = registry.bind(&global)?;
    metadata.set_property(
        0,
        "default.configured.audio.source",
        Some("Spa:String:JSON"),
        Some(&serde_json::json!({ "name": node_name }).to_string()),
    );

    // Roundtrip 2: flush the write before the connection drops.
    *lock_or_recover(&pending) = Some(core.sync(0)?);
    mainloop.run();

    Ok(())
}

/// Resolve an `Audio/Source` node's current registry id by its `node.name`.
/// Ids are scoped to one PipeWire instance, so the name is the durable
/// handle. A missing node is `Ok(None)`, not an error.
///
/// # Errors
/// Returns an error if the PipeWire connection fails.
pub fn find_node_id(node_name: &str) -> Result<Option<u32>, Box<dyn Error>> {
    let node_name = node_name.to_string();
    let ids = collect_globals(move |global| {
        if global.type_ != ObjectType::Node {
            return None;
        }
        let props = global.props.as_ref()?;
        (props.get("media.class") == Some("Audio/Source")
            && props.get("node.name") == Some(node_name.as_str()))
        .then_some(global.id)
    })?;
    Ok(ids.into_iter().next())
}

/// Get the PipeWire node name for a device by its ID, straight from the
/// registry — `node.name` is what streams and recorders target.
///
/// # Errors
/// Returns an error if the PipeWire connection fails or no node with that
/// id exposes a `node.name`.
pub fn get_node_name(node_id: u32) -> Result<String, Box<dyn Error>> {
    let names = collect_globals(move |global| {
        if global.type_ != ObjectType::Node || global.id != node_id {
            return None;
        }
        global.props.as_ref()?.get("node.name").map(str::to_string)
    })?;

    names
        .into_iter()
        .next()
        .ok_or_else(|| format!("Could not find node.name for node {}", node_id).into())
}
