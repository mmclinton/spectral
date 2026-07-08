//! Throwaway P0-4 spike (deleted at the end of the series): a standalone
//! native virtual microphone playing a 440 Hz tone.
//!
//! Architecture proven on this machine 2026-07-07 (the earlier
//! stream-with-`media.class = Audio/Source/Virtual` variant yields a
//! port-less node that nothing can link to on PipeWire 1.4.9 /
//! WirePlumber 0.5.15):
//! - an adapter node (`support.null-audio-sink`) classified
//!   `Audio/Source/Virtual` — lists as an input device only, never as an
//!   output,
//! - a plain playback stream connected without AUTOCONNECT (the session
//!   manager refuses to route playback into a source-classed node, so
//!   autoconnect would fall back to the speakers),
//! - an explicitly created link feeding the stream into the node.
//!
//! All three objects are proxy-owned and vanish when the process exits.
//!
//! Run with `cargo run --example virtual_source_spike`, then check that
//! GNOME Settings / pavucontrol list "Spectral-Spike" as an input only (no
//! new output device), that `pw-record --target Spectral-Spike` captures
//! the tone, and that the node vanishes on Ctrl-C.

use pipewire::spa::param::audio::{AudioFormat, AudioInfoRaw, MAX_CHANNELS};
use pipewire::spa::pod::Pod;
use pipewire::spa::utils::Direction;
use pipewire::types::ObjectType;
use pipewire::{
    context::ContextBox,
    main_loop::MainLoopBox,
    properties::properties,
    stream::{StreamBox, StreamFlags},
};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

const RATE: u32 = 48000;
const FREQUENCY: f64 = 440.0;
const VOLUME: f32 = 0.6;
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(5);

fn main() -> Result<(), Box<dyn std::error::Error>> {
    pipewire::init();
    let mainloop = MainLoopBox::new(None)?;
    let context = ContextBox::new(mainloop.loop_(), None)?;
    let core = context.connect(None)?;

    let _mic = core.create_object::<pipewire::node::Node>(
        "adapter",
        &properties! {
            "factory.name" => "support.null-audio-sink",
            *pipewire::keys::NODE_NAME => "Spectral-Spike",
            *pipewire::keys::NODE_DESCRIPTION => "Spectral-Spike",
            *pipewire::keys::MEDIA_CLASS => "Audio/Source/Virtual",
            *pipewire::keys::NODE_VIRTUAL => "true",
            "audio.position" => "[ MONO ]",
        },
    )?;

    let feed_props = properties! {
        *pipewire::keys::MEDIA_TYPE => "Audio",
        *pipewire::keys::MEDIA_CATEGORY => "Playback",
        *pipewire::keys::MEDIA_ROLE => "Communication",
        *pipewire::keys::NODE_NAME => "Spectral-Spike-feed",
        *pipewire::keys::NODE_DESCRIPTION => "Spectral-Spike-feed",
        *pipewire::keys::NODE_LATENCY => "480/48000",
    };
    let stream = StreamBox::new(&core, "spectral-spike-feed", feed_props)?;

    let _listener = stream
        .add_local_listener_with_user_data(0.0f64)
        .state_changed(|_stream, _phase, old, new| {
            println!("feed stream state: {old:?} -> {new:?}");
        })
        .process(|stream, phase| {
            if let Some(mut buffer) = stream.dequeue_buffer() {
                let requested = buffer.requested() as usize;
                let datas = buffer.datas_mut();
                if datas.is_empty() {
                    return;
                }
                let data = &mut datas[0];
                let samples = if let Some(slice) = data.data() {
                    let max_samples = slice.len() / 4;
                    let samples = if requested > 0 {
                        requested.min(max_samples)
                    } else {
                        max_samples
                    };
                    for i in 0..samples {
                        *phase = (*phase + std::f64::consts::TAU * FREQUENCY / f64::from(RATE))
                            % std::f64::consts::TAU;
                        let value = phase.sin() as f32 * VOLUME;
                        slice[i * 4..i * 4 + 4].copy_from_slice(&value.to_le_bytes());
                    }
                    samples
                } else {
                    0
                };
                let chunk = data.chunk_mut();
                *chunk.offset_mut() = 0;
                *chunk.stride_mut() = 4;
                *chunk.size_mut() = (samples * 4) as u32;
            }
        })
        .register()?;

    let mut audio_info = AudioInfoRaw::new();
    audio_info.set_format(AudioFormat::F32LE);
    audio_info.set_rate(RATE);
    audio_info.set_channels(1);
    let mut position = [0; MAX_CHANNELS];
    position[0] = pipewire::spa::sys::SPA_AUDIO_CHANNEL_MONO;
    audio_info.set_position(position);

    let values: Vec<u8> = pipewire::spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &pipewire::spa::pod::Value::Object(pipewire::spa::pod::Object {
            type_: pipewire::spa::sys::SPA_TYPE_OBJECT_Format,
            id: pipewire::spa::param::ParamType::EnumFormat.as_raw(),
            properties: audio_info.into(),
        }),
    )
    .map_err(|e| format!("failed to serialize the audio format: {e:?}"))?
    .0
    .into_inner();
    let mut params = [Pod::from_bytes(&values).ok_or("invalid format pod")?];

    stream.connect(
        Direction::Output,
        None,
        StreamFlags::MAP_BUFFERS | StreamFlags::RT_PROCESS,
        &mut params,
    )?;

    // The session manager configures both nodes asynchronously; wait for the
    // mic's input port and the feed's output port, then link them ourselves.
    let mic_node: Rc<Cell<Option<u32>>> = Rc::new(Cell::new(None));
    let ports: Rc<RefCell<Vec<(u32, u32, String)>>> = Rc::new(RefCell::new(Vec::new()));

    let registry = core.get_registry()?;
    let mic_node_slot = mic_node.clone();
    let ports_slot = ports.clone();
    let _registry_listener = registry
        .add_listener_local()
        .global(move |global| {
            let Some(props) = global.props else { return };
            match global.type_ {
                ObjectType::Node if props.get("node.name") == Some("Spectral-Spike") => {
                    mic_node_slot.set(Some(global.id));
                }
                ObjectType::Port => {
                    if let (Some(node), Some(direction)) =
                        (props.get("node.id"), props.get("port.direction"))
                        && let Ok(node) = node.parse::<u32>()
                    {
                        ports_slot
                            .borrow_mut()
                            .push((global.id, node, direction.to_string()));
                    }
                }
                _ => {}
            }
        })
        .register();

    let deadline = Instant::now() + DISCOVERY_TIMEOUT;
    let _link = loop {
        if Instant::now() > deadline {
            return Err("timed out waiting for the mic node and stream ports".into());
        }
        mainloop.loop_().iterate(Duration::from_millis(50));

        let Some(mic_id) = mic_node.get() else {
            continue;
        };
        let feed_id = stream.node_id();
        if feed_id == u32::MAX {
            continue;
        }
        let (feed_port, mic_port) = {
            let ports = ports.borrow();
            (
                ports
                    .iter()
                    .find(|(_, node, dir)| *node == feed_id && dir.as_str() == "out")
                    .map(|(id, ..)| *id),
                ports
                    .iter()
                    .find(|(_, node, dir)| *node == mic_id && dir.as_str() == "in")
                    .map(|(id, ..)| *id),
            )
        };
        if let (Some(feed_port), Some(mic_port)) = (feed_port, mic_port) {
            println!("linking feed {feed_id}:{feed_port} -> mic {mic_id}:{mic_port}");
            break core.create_object::<pipewire::link::Link>(
                "link-factory",
                &properties! {
                    "link.output.node" => feed_id.to_string(),
                    "link.output.port" => feed_port.to_string(),
                    "link.input.node" => mic_id.to_string(),
                    "link.input.port" => mic_port.to_string(),
                },
            )?;
        }
    };

    println!("virtual mic \"Spectral-Spike\" is up; Ctrl-C to remove it");
    mainloop.run();
    Ok(())
}
