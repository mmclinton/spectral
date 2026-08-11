//! PipeWire registry queries: enumerating input sources, resolving node
//! names, and pointing the session manager's default source at the mic.
//! Each query opens its own short-lived PipeWire connection.

use crate::state::{AudioDevice, lock_or_recover};
use pipewire as pw;
use pw::core::PW_ID_CORE;
use pw::metadata::Metadata;
use pw::node::{Node, NodeListener};
use pw::properties::PropertiesBox;
use pw::proxy::ProxyT;
use pw::registry::GlobalObject;
use pw::spa::utils::dict::DictRef;
use pw::spa::utils::result::AsyncSeq;
use pw::{context::ContextRc, main_loop::MainLoopRc, types::ObjectType};
use std::cell::RefCell;
use std::collections::HashSet;
use std::error::Error;
use std::rc::Rc;
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

    // Quitting only on `done` would hang forever if the connection dies
    // mid-roundtrip (the daemon calls this while holding the virtual-mic
    // lock, so a hang here wedges every later command). Any core/proxy
    // error therefore aborts the query.
    let error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let error_clone = error.clone();
    let error_loop = mainloop.clone();

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
        .error(move |id, _seq, res, message| {
            let mut slot = lock_or_recover(&error_clone);
            if slot.is_none() {
                *slot = Some(format!("PipeWire error on object {id}: {res} {message}"));
            }
            error_loop.quit();
        })
        .register();

    let seq = core.sync(0)?;
    *lock_or_recover(&pending) = Some(seq);

    mainloop.run();

    if let Some(msg) = lock_or_recover(&error).take() {
        return Err(msg.into());
    }

    let items = std::mem::take(&mut *lock_or_recover(&found));
    Ok(items)
}

/// Channel count when the node doesn't say: stereo, matching PipeWire's
/// own default for unconfigured audio.
const FALLBACK_CHANNELS: u32 = 2;

/// Parse an `audio.channels` property value. Absent or unparsable means
/// "unknown" and yields the fallback; a literal zero is clamped to mono
/// rather than trusted.
fn parse_channel_count(raw: Option<&str>) -> u32 {
    raw.and_then(|s| s.parse::<u32>().ok())
        .map(|n| n.max(1))
        .unwrap_or(FALLBACK_CHANNELS)
}

/// One `Audio/Source` node with its identity and true channel count.
struct SourceNode {
    id: u32,
    node_name: String,
    display_name: String,
    channels: u32,
}

/// Enumerate every `Audio/Source` node and read its real channel count by
/// binding the node — `audio.channels` is never on the registry global,
/// only in the bound node's info props. Two-phase barrier on one transient
/// connection: the first `done` means all globals are seen and all binds
/// issued, so a second sync is queued behind every pending info event;
/// the second `done` means those events have all been delivered.
///
/// A node that dies between its global event and our bind keeps the
/// fallback count instead of failing the whole enumeration; any error on
/// the core or registry still aborts, because a wedged query here would
/// wedge the daemon (same rationale as `collect_globals`).
fn collect_source_nodes() -> Result<Vec<SourceNode>, Box<dyn Error>> {
    let mainloop = MainLoopRc::new(None)?;
    let context = ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;
    let registry = core.get_registry_rc()?;

    let found: Rc<RefCell<Vec<SourceNode>>> = Rc::new(RefCell::new(Vec::new()));
    // Proxies and listeners must outlive the loop: dropping either side
    // cancels the pending info event. Listener before proxy in the tuple —
    // fields drop in declaration order, and a listener unlinks its hook
    // from a list that `pw_proxy_destroy` has already freed if the proxy
    // goes first.
    let bindings: Rc<RefCell<Vec<(NodeListener, Node)>>> = Rc::new(RefCell::new(Vec::new()));
    let node_proxy_ids: Rc<RefCell<HashSet<u32>>> = Rc::new(RefCell::new(HashSet::new()));

    let registry_weak = registry.downgrade();
    let found_globals = found.clone();
    let bindings_globals = bindings.clone();
    let proxy_ids_globals = node_proxy_ids.clone();

    let _reg_listener = registry
        .add_listener_local()
        .global(move |global| {
            if global.type_ != ObjectType::Node {
                return;
            }
            let Some(props) = global.props.as_ref() else {
                return;
            };
            if props.get("media.class") != Some("Audio/Source") {
                return;
            }
            let Some(node_name) = props.get("node.name") else {
                return;
            };
            // An interface split into individual inputs gives every one
            // the same `node.nick` (the card, "EVO4") and distinguishes
            // them only in `node.description` ("EVO4 Mic 1 / Line 1").
            let display_name = props
                .get("node.description")
                .or_else(|| props.get("node.nick"))
                .unwrap_or("Unknown")
                .to_string();

            let index = {
                let mut entries = found_globals.borrow_mut();
                entries.push(SourceNode {
                    id: global.id,
                    node_name: node_name.to_string(),
                    display_name,
                    channels: FALLBACK_CHANNELS,
                });
                entries.len() - 1
            };

            let Some(registry) = registry_weak.upgrade() else {
                return;
            };
            // A failed bind (node already gone) leaves the fallback entry.
            let Ok(node) = registry.bind::<Node, _>(global) else {
                return;
            };
            proxy_ids_globals
                .borrow_mut()
                .insert(node.upcast_ref().id());
            let found_info = found_globals.clone();
            let listener = node
                .add_listener_local()
                .info(move |info| {
                    if let Some(props) = info.props()
                        && let Some(entry) = found_info.borrow_mut().get_mut(index)
                    {
                        entry.channels = parse_channel_count(props.get("audio.channels"));
                    }
                })
                .register();
            bindings_globals.borrow_mut().push((listener, node));
        })
        .register();

    // first: flushes registry globals (and, since binds are issued during
    // global delivery, orders them all before the second sync request).
    // second: flushes the info events those binds provoked.
    struct PendingSyncs {
        first: Option<AsyncSeq>,
        second: Option<AsyncSeq>,
    }
    let pending = Rc::new(RefCell::new(PendingSyncs {
        first: None,
        second: None,
    }));
    let error: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));

    let core_weak = core.downgrade();
    let loop_done = mainloop.clone();
    let pending_done = pending.clone();
    let error_done = error.clone();
    let loop_err = mainloop.clone();
    let error_err = error.clone();
    let proxy_ids_err = node_proxy_ids.clone();

    let _core_listener = core
        .add_listener_local()
        .done(move |id, seq| {
            if id != PW_ID_CORE {
                return;
            }
            let mut p = pending_done.borrow_mut();
            if p.first == Some(seq) {
                p.first = None;
                match core_weak.upgrade().map(|core| core.sync(0)) {
                    Some(Ok(second)) => p.second = Some(second),
                    _ => {
                        *error_done.borrow_mut() =
                            Some("PipeWire sync failed mid-enumeration".to_string());
                        loop_done.quit();
                    }
                }
            } else if p.second == Some(seq) {
                p.second = None;
                loop_done.quit();
            }
        })
        .error(move |id, _seq, res, message| {
            // A bound node dying mid-query is expected churn — its entry
            // keeps the fallback. Core/registry errors stay fatal.
            if proxy_ids_err.borrow().contains(&id) {
                return;
            }
            let mut slot = error_err.borrow_mut();
            if slot.is_none() {
                *slot = Some(format!("PipeWire error on object {id}: {res} {message}"));
            }
            loop_err.quit();
        })
        .register();

    pending.borrow_mut().first = Some(core.sync(0)?);
    mainloop.run();

    if let Some(msg) = error.borrow_mut().take() {
        return Err(msg.into());
    }
    let items = std::mem::take(&mut *found.borrow_mut());
    Ok(items)
}

/// Enumerates `Audio/Source` nodes with their true channel counts,
/// skipping monitor sources; display names prefer `node.description`
/// over the coarser `node.nick`.
pub fn get_input_devices() -> Result<Vec<AudioDevice>, Box<dyn Error>> {
    Ok(collect_source_nodes()?
        .into_iter()
        .filter(|node| !node.display_name.to_lowercase().contains("monitor"))
        .map(|node| AudioDevice {
            id: node.id,
            name: node.display_name,
            channels: node.channels,
        })
        .collect())
}

/// Resolve an `Audio/Source` node's `node.name` and true channel count by
/// id in one pass — enable needs both.
///
/// # Errors
/// Returns an error if the PipeWire connection fails or no `Audio/Source`
/// node has that id.
pub fn get_node_details(node_id: u32) -> Result<(String, u32), Box<dyn Error>> {
    collect_source_nodes()?
        .into_iter()
        .find(|node| node.id == node_id)
        .map(|node| (node.node_name, node.channels))
        .ok_or_else(|| format!("No Audio/Source node with id {node_id}").into())
}

/// Point the configured default input at `node_name` — the persisted
/// equivalent of `wpctl set-default`. WirePlumber stores the value and
/// re-applies it whenever a node with that name (re)appears.
pub fn set_default_source(node_name: &str) -> Result<(), Box<dyn Error>> {
    let mainloop = MainLoopRc::new(None)?;
    let context = ContextRc::new(&mainloop, None)?;
    let core = context.connect(Some(super::manager_client_properties()))?;
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

    // Same rationale as collect_globals: without an error path out of the
    // loop, a connection that dies mid-roundtrip hangs this thread forever.
    let error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let error_clone = error.clone();
    let error_loop = mainloop.clone();

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
        .error(move |id, _seq, res, message| {
            let mut slot = lock_or_recover(&error_clone);
            if slot.is_none() {
                *slot = Some(format!("PipeWire error on object {id}: {res} {message}"));
            }
            error_loop.quit();
        })
        .register();

    // Roundtrip 1: enumerate the globals so the session manager's "default"
    // metadata object is discovered.
    *lock_or_recover(&pending) = Some(core.sync(0)?);
    mainloop.run();

    if let Some(msg) = lock_or_recover(&error).take() {
        return Err(msg.into());
    }

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

    if let Some(msg) = lock_or_recover(&error).take() {
        return Err(msg.into());
    }

    Ok(())
}

/// Resolve an `Audio/Source` node's current registry id and true channel
/// count by its `node.name`. Ids are scoped to one PipeWire instance, so
/// the name is the durable handle. A missing node is `Ok(None)`, not an
/// error.
///
/// # Errors
/// Returns an error if the PipeWire connection fails.
pub fn find_node(node_name: &str) -> Result<Option<(u32, u32)>, Box<dyn Error>> {
    Ok(collect_source_nodes()?
        .into_iter()
        .find(|node| node.node_name == node_name)
        .map(|node| (node.id, node.channels)))
}

/// [`find_node`] reduced to the id, for callers that only need presence.
pub fn find_node_id(node_name: &str) -> Result<Option<u32>, Box<dyn Error>> {
    Ok(find_node(node_name)?.map(|(id, _)| id))
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
        .ok_or_else(|| format!("Could not find node.name for node {node_id}").into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_count_parsing_reads_clamps_zero_and_falls_back() {
        let cases: [(Option<&str>, u32); 9] = [
            (None, 2),
            (Some("1"), 1),
            (Some("2"), 2),
            (Some("8"), 8),
            (Some("16"), 16),
            (Some("0"), 1),
            (Some(""), 2),
            (Some("abc"), 2),
            (Some("-1"), 2),
        ];
        for (raw, expected) in cases {
            assert_eq!(parse_channel_count(raw), expected, "raw {raw:?}");
        }
    }
}
