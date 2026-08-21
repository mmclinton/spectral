//! Native PipeWire Audio Pipeline
//!
//! This module provides a native PipeWire client for audio capture and playback,
//! using direct stream connections for low-latency noise reduction.

use super::audio_processor::AudioProcessor;
use crate::state::lock_or_recover;
use pipewire::spa::{
    param::ParamType,
    param::audio::{AudioFormat, AudioInfoRaw, MAX_CHANNELS},
    param::format::{MediaSubtype, MediaType},
    param::format_utils,
    pod::Pod,
    utils::Direction,
};
use pipewire::{
    context::ContextBox,
    link::Link,
    main_loop::MainLoopBox,
    node::Node,
    properties::properties,
    stream::{StreamBox, StreamFlags, StreamState},
    types::ObjectType,
};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// Audio sample rate in Hz (48kHz is standard for PipeWire)
pub const SAMPLE_RATE: u32 = 48000;

/// The virtual mic emits mono; capture is downmixed before processing.
pub const OUTPUT_CHANNELS: u32 = 1;

/// RNNoise requires exactly 480 samples (10ms at 48kHz) per frame.
pub const RNNOISE_FRAME_SIZE: usize = 480;

/// Ceiling on capture channels. Wider devices are captured as their first
/// eight channels — an unpositioned offer pairs channels by index, so the
/// clamp keeps native order (channelmix `pair_mix`). Validated on 1- and
/// 2-channel devices; wider layouts are untested but fail closed via the
/// Format check in the capture listener.
pub const MAX_INPUT_CHANNELS: u32 = 8;

/// Scaling factor for converting f32 PCM [-1.0, 1.0] to RNNoise's expected i16 range.
/// RNNoise expects samples in the range [-32768.0, 32767.0] as f32 values.
pub const RNNOISE_SCALE_FACTOR: f32 = 32768.0;

/// Maximum time to wait for the pipeline thread to finish its PipeWire setup.
pub(crate) const PIPELINE_READY_TIMEOUT: Duration = Duration::from_secs(5);

/// Backstop bound on the capture→feed buffer (1 s of mono audio). The linked
/// virtual-mic node drives its own graph section, so the feed drains
/// continuously and this should be unreachable in practice.
const OUTPUT_BUFFER_CAP: usize = SAMPLE_RATE as usize;

/// Largest per-callback delivery the buffers reserve for, in samples per
/// channel — PipeWire's default `clock.max-quantum`. A graph configured
/// beyond it still works (the Vec grows once), but every stock
/// configuration stays allocation-free on the RT path.
const MAX_QUANTUM: usize = 8192;

/// Ready = both streams up and the feed link created; the daemon's Enable
/// blocks on this inside PIPELINE_READY_TIMEOUT.
#[derive(Default)]
struct ReadyFlags {
    capture: Cell<bool>,
    feed: Cell<bool>,
}

/// What negotiation settled on, shared between the `param_changed` handler
/// and the capture `process` callback.
///
/// Atomic, and deliberately *not* fields of [`PipelineData`], because those
/// two callbacks run on different threads: `param_changed` is a main-loop
/// event while `process` runs on a PipeWire data thread under `RT_PROCESS`.
/// `RefCell`'s borrow flag is a plain `Cell`, so a Format arriving while the
/// data thread is mid-cycle would either race it — two live `&mut` to the
/// same struct — or panic `BorrowMutError` out of an `extern "C"` callback,
/// which aborts the daemon rather than unwinding. Atomics also give the halt
/// a defined visibility across that boundary, which a plain `bool` did not.
///
/// `SeqCst` throughout: these are read once per delivery, not per sample, so
/// the ordering costs nothing measurable and nobody has to reason about
/// which pairs need acquire/release.
///
/// Held in an `Arc` rather than the `Rc` the rest of this module uses for
/// callback state. Everything else here is shared only between callbacks the
/// PipeWire loop runs on one thread; this is the one thing deliberately
/// crossing to the data thread, and `Rc`'s refcount is not atomic even when
/// its contents are. The clones all happen on the pipeline thread today, so
/// `Rc` would work — but "works because nobody clones it from the RT side"
/// is an invariant an audit has to re-establish, and `Arc` costs one
/// allocation at startup to make it a property of the type.
struct FormatState {
    /// Channel count negotiation granted — the stride the deinterleave uses.
    channels: AtomicU32,
    /// Set once the first Format locked `channels`; a later Format changing
    /// it halts the pipeline.
    locked: AtomicBool,
    /// Poisoned by an unusable or renegotiated format: the process callback
    /// drops deliveries rather than deinterleave with a wrong stride.
    halted: AtomicBool,
}

impl FormatState {
    fn new(channels: u32) -> Self {
        Self {
            channels: AtomicU32::new(channels),
            locked: AtomicBool::new(false),
            halted: AtomicBool::new(false),
        }
    }
}

struct PipelineData {
    processor: AudioProcessor,
    input_buffer: Vec<f32>,
    output_buffer: Vec<f32>,
    mono_scratch: Vec<f32>,
    processed_scratch: Vec<f32>,
}

impl PipelineData {
    fn new(processor: AudioProcessor) -> Self {
        // Full worst-case reservations: the process callbacks push into
        // these on the RT thread, so growth there would mean an RT-path
        // malloc at exactly the wrong moment (e.g. a stalled consumer).
        // Input holds at most one leftover partial frame plus one maximal
        // delivery; output is trimmed to OUTPUT_BUFFER_CAP only after a
        // whole delivery is processed, so it transiently exceeds the cap
        // by up to one delivery's worth of mono frames.
        //
        // Sized for the ceiling, not the request: the granted channel
        // count arrives only with the Format param, after allocation.
        let input_capacity = (MAX_QUANTUM + RNNOISE_FRAME_SIZE) * MAX_INPUT_CHANNELS as usize;
        let output_capacity = OUTPUT_BUFFER_CAP + MAX_QUANTUM;
        Self {
            processor,
            input_buffer: Vec::with_capacity(input_capacity),
            output_buffer: Vec::with_capacity(output_capacity),
            mono_scratch: vec![0.0; RNNOISE_FRAME_SIZE],
            processed_scratch: vec![0.0; RNNOISE_FRAME_SIZE],
        }
    }
}

/// Handle to the dedicated audio thread running the PipeWire mainloop.
/// Every PipeWire object — capture stream, virtual mic node, feed link —
/// lives on that thread and dies with it.
pub struct NativeAudioPipeline {
    running: Arc<AtomicBool>,
    failure: Arc<Mutex<Option<String>>>,
    pipeline_thread: Option<JoinHandle<()>>,
}

/// Why a granted format cannot be decoded, or `None` if it can.
///
/// Both process callbacks read 4-byte little-endian samples with a stride
/// compiled in, and RNNoise's frame is 10 ms only at [`SAMPLE_RATE`] — so an
/// encoding or rate we did not ask for cannot be reinterpreted, only
/// refused. This is the same reasoning the channel check rests on, applied
/// to the other two fields `AudioInfoRaw` carries.
///
/// `AudioFormat` has no `Debug`, hence the raw discriminant in the message;
/// it maps to `SPA_AUDIO_FORMAT_*` in `spa/param/audio/raw.h`.
fn unsupported_encoding(info: &AudioInfoRaw) -> Option<String> {
    if info.format() != AudioFormat::F32LE {
        return Some(format!("sample format {} is not F32LE", info.format().0));
    }
    if info.rate() != SAMPLE_RATE {
        return Some(format!("{} Hz is not {SAMPLE_RATE}", info.rate()));
    }
    None
}

/// First error wins: later failures are usually cascades of the first.
fn record_failure(slot: &Mutex<Option<String>>, msg: String) {
    let mut guard = lock_or_recover(slot);
    if guard.is_none() {
        *guard = Some(msg);
    }
}

impl NativeAudioPipeline {
    /// Start the native audio pipeline: capture `source_node` with
    /// `input_channels` channels, denoise, and feed the virtual mic
    /// named (and displayed as) `virtual_name`.
    pub fn start(
        processor: AudioProcessor,
        source_node: &str,
        virtual_name: &str,
        input_channels: u32,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let running = Arc::new(AtomicBool::new(true));
        let running_clone = running.clone();
        let failure = Arc::new(Mutex::new(None));
        let failure_clone = failure.clone();
        let source_node = source_node.to_string();
        let virtual_name = virtual_name.to_string();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();

        let pipeline_thread = thread::spawn(move || {
            if let Err(e) = run_pipeline(
                processor,
                &source_node,
                &virtual_name,
                running_clone,
                input_channels,
                &ready_tx,
                &failure_clone,
            ) {
                record_failure(&failure_clone, format!("pipeline error: {e}"));
                let _ = ready_tx.send(Err(e.to_string()));
                eprintln!("[pipeline] pipeline error: {e}");
            }
        });

        match ready_rx.recv_timeout(PIPELINE_READY_TIMEOUT) {
            Ok(Ok(())) => Ok(Self {
                running,
                failure,
                pipeline_thread: Some(pipeline_thread),
            }),
            Ok(Err(e)) => {
                let _ = pipeline_thread.join();
                Err(format!("Pipeline setup failed: {e}").into())
            }
            Err(_) => {
                // Don't join: a wedged setup thread would hang the daemon.
                running.store(false, Ordering::SeqCst);
                Err(
                    format!("Pipeline did not become ready within {PIPELINE_READY_TIMEOUT:?}")
                        .into(),
                )
            }
        }
    }

    /// Signals the pipeline thread to quit and joins it; a second call is
    /// a no-op.
    pub fn stop(&mut self) {
        self.running.store(false, Ordering::SeqCst);

        if let Some(handle) = self.pipeline_thread.take()
            && handle.join().is_err()
        {
            eprintln!("Pipeline thread panicked during shutdown");
        }
    }

    /// Why the pipeline died, if it has. A finished thread with an empty
    /// failure slot (e.g. a panic) still counts while `running` is set.
    pub fn failure(&self) -> Option<String> {
        if let Some(msg) = lock_or_recover(&self.failure).clone() {
            return Some(msg);
        }
        let thread_exited = self
            .pipeline_thread
            .as_ref()
            .is_some_and(JoinHandle::is_finished);
        (self.running.load(Ordering::SeqCst) && thread_exited)
            .then(|| "pipeline thread exited unexpectedly".to_string())
    }
}

impl Drop for NativeAudioPipeline {
    fn drop(&mut self) {
        if self.pipeline_thread.is_some() {
            self.stop();
        }
    }
}

pub(crate) fn build_audio_format_pod(
    format: AudioFormat,
    rate: u32,
    channels: u32,
    positions: Option<&[u32]>,
) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    let mut audio_info = AudioInfoRaw::new();
    audio_info.set_format(format);
    audio_info.set_rate(rate);
    audio_info.set_channels(channels);
    if let Some(positions) = positions {
        let mut position = [0u32; MAX_CHANNELS];
        position[..positions.len()].copy_from_slice(positions);
        audio_info.set_position(position);
    }

    let values: Vec<u8> = pipewire::spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &pipewire::spa::pod::Value::Object(pipewire::spa::pod::Object {
            type_: pipewire::spa::sys::SPA_TYPE_OBJECT_Format,
            id: pipewire::spa::param::ParamType::EnumFormat.as_raw(),
            properties: audio_info.into(),
        }),
    )
    .map_err(|e| format!("Failed to serialize audio format: {e:?}"))?
    .0
    .into_inner();

    Ok(values)
}

fn run_pipeline(
    processor: AudioProcessor,
    source_node: &str,
    virtual_name: &str,
    running: Arc<AtomicBool>,
    input_channels: u32,
    ready_tx: &mpsc::Sender<Result<(), String>>,
    failure: &Arc<Mutex<Option<String>>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    pipewire::init();

    let mainloop = MainLoopBox::new(None)?;
    let context = ContextBox::new(mainloop.loop_(), None)?;
    let core = context.connect(Some(super::manager_client_properties()))?;

    let channels = input_channels.clamp(1, MAX_INPUT_CHANNELS);

    // Must use Rc because PipeWire callbacks are !Send
    let pipeline_data = Rc::new(RefCell::new(PipelineData::new(processor)));
    let format_state = Arc::new(FormatState::new(channels));

    let capture_format = build_audio_format_pod(AudioFormat::F32LE, SAMPLE_RATE, channels, None)?;
    let feed_format = build_audio_format_pod(
        AudioFormat::F32LE,
        SAMPLE_RATE,
        OUTPUT_CHANNELS,
        Some(&[pipewire::spa::sys::SPA_AUDIO_CHANNEL_MONO]),
    )?;

    // The mic itself: an adapter node classified as a virtual source. It
    // lists as an input device only — no sink, no monitor — and dies with
    // this connection. WirePlumber refuses to route playback into a
    // source-classed node, so the feed stream below connects unrouted and
    // the link is created explicitly once both ports exist.
    let _virtual_mic_node = core.create_object::<Node>(
        "adapter",
        &properties! {
            "factory.name" => "support.null-audio-sink",
            *pipewire::keys::NODE_NAME => virtual_name,
            *pipewire::keys::NODE_DESCRIPTION => virtual_name,
            *pipewire::keys::MEDIA_CLASS => "Audio/Source/Virtual",
            *pipewire::keys::NODE_VIRTUAL => "true",
            "audio.position" => "[ MONO ]",
            "device.icon-name" => "audio-input-microphone",
        },
    )?;

    let ready = Rc::new(ReadyFlags::default());

    // "target.object" used since NODE_TARGET isn't exposed in the Rust bindings
    let capture_props = properties! {
        *pipewire::keys::MEDIA_TYPE => "Audio",
        *pipewire::keys::MEDIA_CATEGORY => "Capture",
        *pipewire::keys::MEDIA_ROLE => "Communication",
        *pipewire::keys::NODE_NAME => "spectral-native-capture",
        *pipewire::keys::NODE_DESCRIPTION => "Spectral Native Capture",
        *pipewire::keys::NODE_LATENCY => "480/48000",
        "target.object" => source_node,
        // Two properties covering two halves of one hazard, neither a
        // substitute for the other. Without them a target that goes away —
        // unplugged interface, suspended device, a Bluetooth mic dropping —
        // makes WirePlumber relink us to whatever the default source now
        // is, and the virtual mic keeps denoising a microphone the user
        // never chose, with `failure` empty and the pill still Active.
        //
        // `dont-reconnect` stops the move but reports nothing: the policy
        // takes an early exit for an item that was already linked, which
        // skips the branch that would error. On its own it trades a wrong
        // mic for a silent one. `dont-fallback` is what makes an
        // unresolvable target.object a client error and a destroyed node.
        // Validated by unplugging a live capture device: it arrives as
        // `StreamState::Error("defined target not found")` on the handler
        // above — a stream error, not a core one, so the failure slot is
        // reached through state_changed and the pill goes Inactive.
        "node.dont-reconnect" => "true",
        "node.dont-fallback" => "true",
    };

    let capture_stream = StreamBox::new(&core, "spectral-capture", capture_props)?;

    let capture_data = pipeline_data.clone();
    let capture_failure = failure.clone();
    let param_failure = failure.clone();
    let capture_ready = ready.clone();
    // Each callback carries its own handle rather than reaching through the
    // listener's user data, which is what keeps the main-loop handler off
    // the RT callback's `RefCell`. See [`FormatState`].
    let param_format = format_state.clone();
    let capture_format_state = format_state.clone();
    let _capture_listener = capture_stream
        .add_local_listener_with_user_data(capture_data)
        .state_changed(move |_stream, _data, _old, new| match new {
            StreamState::Error(msg) => {
                record_failure(&capture_failure, format!("capture stream error: {msg}"));
            }
            StreamState::Paused | StreamState::Streaming => capture_ready.capture.set(true),
            _ => {}
        })
        // The granted format, not the offered one: PipeWire's adapter is
        // free to convert, so the stride the deinterleave uses has to come
        // from what negotiation actually settled on.
        .param_changed(move |_stream, _data, id, param| {
            if id != ParamType::Format.as_raw() {
                return;
            }
            let Some(param) = param else { return };
            let Ok((media_type, media_subtype)) = format_utils::parse_format(param) else {
                return;
            };
            if media_type != MediaType::Audio || media_subtype != MediaSubtype::Raw {
                return;
            }
            let mut info = AudioInfoRaw::new();
            if info.parse(param).is_err() {
                record_failure(
                    &param_failure,
                    "capture stream: unparsable Format param".to_string(),
                );
                param_format.halted.store(true, Ordering::SeqCst);
                return;
            }
            if let Some(why) = unsupported_encoding(&info) {
                record_failure(&param_failure, format!("capture stream: {why}"));
                param_format.halted.store(true, Ordering::SeqCst);
                return;
            }
            let granted = info.channels();
            if granted == 0 || granted > MAX_INPUT_CHANNELS {
                record_failure(
                    &param_failure,
                    format!("capture stream: {granted} channels outside 1..={MAX_INPUT_CHANNELS}"),
                );
                param_format.halted.store(true, Ordering::SeqCst);
            } else if param_format.locked.load(Ordering::SeqCst)
                && granted != param_format.channels.load(Ordering::SeqCst)
            {
                let locked = param_format.channels.load(Ordering::SeqCst);
                record_failure(
                    &param_failure,
                    format!(
                        "capture stream: format renegotiated from {locked} to {granted} channels mid-stream"
                    ),
                );
                param_format.halted.store(true, Ordering::SeqCst);
            } else {
                // `locked` is read only here, on the main loop, so this pair
                // needs no ordering against the process callback — that side
                // reads `halted` and `channels` and nothing else.
                param_format.channels.store(granted, Ordering::SeqCst);
                param_format.locked.store(true, Ordering::SeqCst);
            }
        })
        .process(move |stream, data| {
            process_capture_buffer(stream, data, &capture_format_state);
        })
        .register()?;

    let capture_pod =
        Pod::from_bytes(&capture_format).ok_or("Failed to create capture format pod")?;
    capture_stream.connect(
        Direction::Input,
        None,
        StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS | StreamFlags::RT_PROCESS,
        &mut [capture_pod],
    )?;

    // No AUTOCONNECT: the session manager would route this to the default
    // sink (audible!) since it refuses source-classed targets.
    let feed_props = properties! {
        *pipewire::keys::MEDIA_TYPE => "Audio",
        *pipewire::keys::MEDIA_CATEGORY => "Playback",
        *pipewire::keys::MEDIA_ROLE => "Communication",
        *pipewire::keys::NODE_NAME => "spectral-native-feed",
        *pipewire::keys::NODE_DESCRIPTION => "Spectral Native Feed",
        *pipewire::keys::NODE_LATENCY => "480/48000",
    };

    let feed_stream = StreamBox::new(&core, "spectral-feed", feed_props)?;

    let feed_data = pipeline_data.clone();
    let feed_failure = failure.clone();
    let feed_param_failure = failure.clone();
    let feed_ready = ready.clone();
    let feed_param_format = format_state.clone();
    let feed_format_state = format_state.clone();
    let _feed_listener = feed_stream
        .add_local_listener_with_user_data(feed_data)
        .state_changed(move |_stream, _data, _old, new| match new {
            StreamState::Error(msg) => {
                record_failure(&feed_failure, format!("feed stream error: {msg}"));
            }
            StreamState::Paused | StreamState::Streaming => feed_ready.feed.set(true),
            _ => {}
        })
        // The output side needs the same guarantee as the input side:
        // `process_source_buffer` writes f32 little-endian at a stride of 4
        // and sizes silence the same way, so a grant it did not ask for
        // would be written at the wrong width rather than refused. One
        // `halted` covers both streams — either format being untrustworthy
        // makes the whole pipeline untrustworthy.
        .param_changed(move |_stream, _data, id, param| {
            if id != ParamType::Format.as_raw() {
                return;
            }
            let Some(param) = param else { return };
            let Ok((media_type, media_subtype)) = format_utils::parse_format(param) else {
                return;
            };
            if media_type != MediaType::Audio || media_subtype != MediaSubtype::Raw {
                return;
            }
            let mut info = AudioInfoRaw::new();
            let why = if info.parse(param).is_err() {
                Some("unparsable Format param".to_string())
            } else if let Some(why) = unsupported_encoding(&info) {
                Some(why)
            } else if info.channels() != OUTPUT_CHANNELS {
                Some(format!(
                    "{} channels is not the {OUTPUT_CHANNELS} the mic emits",
                    info.channels()
                ))
            } else {
                None
            };
            if let Some(why) = why {
                record_failure(&feed_param_failure, format!("feed stream: {why}"));
                feed_param_format.halted.store(true, Ordering::SeqCst);
            }
        })
        .process(move |stream, data| {
            process_source_buffer(stream, data, &feed_format_state);
        })
        .register()?;

    let feed_pod = Pod::from_bytes(&feed_format).ok_or("Failed to create feed format pod")?;
    feed_stream.connect(
        Direction::Output,
        None,
        StreamFlags::MAP_BUFFERS | StreamFlags::RT_PROCESS,
        &mut [feed_pod],
    )?;

    // Any proxy error is fatal: every object this connection owns (core,
    // streams, mic node, feed link) is load-bearing, and create_object
    // reports failures asynchronously as proxy errors.
    let core_failure = failure.clone();
    let _core_listener = core
        .add_listener_local()
        .error(move |id, _seq, res, message| {
            record_failure(
                &core_failure,
                format!("PipeWire error on object {id}: {res} {message}"),
            );
        })
        .register();

    // The session manager configures the mic node and the feed stream
    // asynchronously; watch the registry for their ports, then link them.
    // The port list exists only to create that one link: once it is up,
    // collection stops and the list is dropped — otherwise it would grow
    // for the pipeline's lifetime as devices hot-plug.
    let mic_node_id: Rc<Cell<Option<u32>>> = Rc::new(Cell::new(None));
    let ports: Rc<RefCell<Vec<(u32, u32, String)>>> = Rc::new(RefCell::new(Vec::new()));
    let link_established = Rc::new(Cell::new(false));

    let registry = core.get_registry()?;
    let mic_node_slot = mic_node_id.clone();
    let ports_slot = ports.clone();
    let link_established_slot = link_established.clone();
    let mic_node_name = virtual_name.to_string();
    let _registry_listener = registry
        .add_listener_local()
        .global(move |global| {
            let Some(props) = &global.props else { return };
            match global.type_ {
                ObjectType::Node if props.get("node.name") == Some(mic_node_name.as_str()) => {
                    mic_node_slot.set(Some(global.id));
                }
                ObjectType::Port if !link_established_slot.get() => {
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

    let mut feed_link: Option<Link> = None;
    let mut ready_sent = false;

    while running.load(Ordering::SeqCst) {
        let res = mainloop
            .loop_()
            .iterate(std::time::Duration::from_millis(10));
        if res < 0 {
            let errno = -res;
            record_failure(failure, format!("main loop iterate failed: errno {errno}"));
            break;
        }
        if lock_or_recover(failure).is_some() {
            break;
        }

        if feed_link.is_none()
            && let Some(mic_id) = mic_node_id.get()
        {
            let feed_id = feed_stream.node_id();
            if feed_id != u32::MAX {
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
                    feed_link = Some(core.create_object::<Link>(
                        "link-factory",
                        &properties! {
                            "link.output.node" => feed_id.to_string(),
                            "link.output.port" => feed_port.to_string(),
                            "link.input.node" => mic_id.to_string(),
                            "link.input.port" => mic_port.to_string(),
                        },
                    )?);
                    link_established.set(true);
                    ports.borrow_mut().clear();
                }
            }
        }

        if !ready_sent && feed_link.is_some() && ready.capture.get() && ready.feed.get() {
            ready_sent = true;
            let _ = ready_tx.send(Ok(()));
        }
    }

    // Surface the real reason when setup never completed — without this the
    // daemon only sees the generic ready-timeout.
    if !ready_sent {
        let msg = lock_or_recover(failure)
            .clone()
            .unwrap_or_else(|| "pipeline stopped before becoming ready".to_string());
        let _ = ready_tx.send(Err(msg));
    }

    mainloop.quit();

    Ok(())
}

/// Coerces one decoded sample into the nominal PCM range.
///
/// `f32::from_le_bytes` accepts every bit pattern, so a driver glitch or a
/// hostile graph node can deliver NaN or an infinity. Both latch permanently
/// downstream: the high-pass biquad's denormal snap tests `abs() <
/// STATE_FLOOR`, which is false for NaN, and RNNoise keeps its own poisoned
/// history — so a single bad sample silences the virtual mic until the
/// pipeline is rebuilt. Clamping to ±1.0 additionally closes the path where
/// a finite but absurd sample becomes infinite under the RNNoise scale
/// factor. Branch-only, so it stays RT-safe.
#[inline]
fn sanitize_sample(sample: f32) -> f32 {
    if sample.is_finite() {
        sample.clamp(-1.0, 1.0)
    } else {
        0.0
    }
}

/// Mix N channels down to mono.
///
/// Mono downmix scaled to the i16 range RNNoise expects. Only the first two
/// channels are averaged: on USB interfaces exposing more (the EVO4's
/// surround-4.0 profile is FL FR RL RR), the front pair is the physical
/// preamps while the rest carry loopback/monitor returns — averaging those
/// in buries the mic under attenuation and system-audio bleed.
#[inline]
fn mix_to_mono(input: &[f32], channels: usize, output: &mut [f32]) {
    if channels == 1 {
        for (i, &sample) in input.iter().enumerate() {
            if i >= output.len() {
                break;
            }
            output[i] = sample * RNNOISE_SCALE_FACTOR;
        }
        return;
    }

    let frame_count = input.len() / channels;
    let used = channels.min(2);
    let channel_divisor = used as f32;

    for i in 0..frame_count {
        if i >= output.len() {
            break;
        }

        let sum: f32 = (0..used)
            .map(|ch| {
                let idx = i * channels + ch;
                if idx < input.len() { input[idx] } else { 0.0 }
            })
            .sum();

        // Scale to i16 range for RNNoise
        output[i] = (sum / channel_divisor) * RNNOISE_SCALE_FACTOR;
    }
}

fn process_capture_buffer(
    stream: &pipewire::stream::Stream,
    data: &mut Rc<RefCell<PipelineData>>,
    format: &FormatState,
) {
    if let Some(mut buffer) = stream.dequeue_buffer() {
        let datas = buffer.datas_mut();
        if datas.is_empty() {
            return;
        }

        let chunk = datas[0].chunk();
        let chunk_offset = chunk.offset() as usize;
        let chunk_size = chunk.size() as usize;

        if let Some(slice) = datas[0].data() {
            if chunk_size == 0 || chunk_offset + chunk_size > slice.len() {
                return;
            }

            // A format we can't trust: drop the delivery rather than
            // deinterleave it at the wrong stride. The daemon surfaces the
            // recorded failure through its health poll. Checked before the
            // borrow so a halted stream costs two atomic loads and nothing
            // else.
            if format.halted.load(Ordering::SeqCst) {
                return;
            }
            let channels = format.channels.load(Ordering::SeqCst) as usize;

            let valid_data = &slice[chunk_offset..chunk_offset + chunk_size];
            let mut pipeline_data = data.borrow_mut();
            let PipelineData {
                processor,
                input_buffer,
                output_buffer,
                mono_scratch,
                processed_scratch,
            } = &mut *pipeline_data;

            for audio_chunk in valid_data.chunks_exact(4) {
                let sample = f32::from_le_bytes([
                    audio_chunk[0],
                    audio_chunk[1],
                    audio_chunk[2],
                    audio_chunk[3],
                ]);
                input_buffer.push(sanitize_sample(sample));
            }

            // Process complete frames (480 samples per channel) in the
            // preallocated scratch buffers — the RT thread must not
            // allocate. Frames advance by offset with one drain at the end:
            // draining per frame would shift the whole remainder each
            // iteration, quadratic in the number of buffered frames.
            let frame_size = RNNOISE_FRAME_SIZE * channels;
            let mut consumed = 0;
            while input_buffer.len() - consumed >= frame_size {
                // Convert N channels to mono and scale to i16 range for RNNoise
                mix_to_mono(
                    &input_buffer[consumed..consumed + frame_size],
                    channels,
                    mono_scratch,
                );
                processor.process_frame(mono_scratch, processed_scratch);

                // Scale back to f32 PCM range [-1.0, 1.0]
                for sample in processed_scratch.iter_mut() {
                    *sample /= RNNOISE_SCALE_FACTOR;
                }

                output_buffer.extend_from_slice(processed_scratch);
                consumed += frame_size;
            }
            input_buffer.drain(..consumed);
            cap_buffer(output_buffer, OUTPUT_BUFFER_CAP);
        }
    }
}

/// Keep only the newest `max` samples; silently drops overflow (backstop
/// against a stalled consumer side — overflow protection, not flow control).
fn cap_buffer(buf: &mut Vec<f32>, max: usize) {
    let excess = buf.len().saturating_sub(max);
    if excess > 0 {
        buf.drain(..excess);
    }
}

fn process_source_buffer(
    stream: &pipewire::stream::Stream,
    data: &mut Rc<RefCell<PipelineData>>,
    format: &FormatState,
) {
    // Same rule as the capture side, one cycle earlier: skip the dequeue
    // entirely rather than write at a width negotiation did not agree to.
    // The consumer sees no buffer this cycle, which is the honest output for
    // a pipeline that cannot trust its format. (The capture side checks
    // *after* its dequeue, because there the buffer is already ours and has
    // to be recycled on Drop.)
    if format.halted.load(Ordering::SeqCst) {
        return;
    }
    if let Some(mut buffer) = stream.dequeue_buffer() {
        // How many frames the graph asks for; must be read before datas_mut.
        let requested = buffer.requested() as usize;
        let datas = buffer.datas_mut();
        if datas.is_empty() {
            return;
        }

        if let Some(slice) = datas[0].data() {
            let mut pipeline_data = data.borrow_mut();

            let max_samples = slice.len() / 4;
            let available = pipeline_data.output_buffer.len().min(max_samples);

            let samples = if available > 0 {
                for (i, sample) in pipeline_data.output_buffer[..available].iter().enumerate() {
                    slice[i * 4..i * 4 + 4].copy_from_slice(&sample.to_le_bytes());
                }
                pipeline_data.output_buffer.drain(..available);
                available
            } else {
                // Underrun (e.g. the first cycles after enable): emit silence
                // sized to the request so the mic never produces an empty cycle.
                let samples = if requested > 0 {
                    requested.min(max_samples)
                } else {
                    max_samples
                };
                slice[..samples * 4].fill(0);
                samples
            };

            // CRITICAL: Set the chunk size to indicate how many bytes we wrote
            let chunk = datas[0].chunk_mut();
            *chunk.offset_mut() = 0;
            *chunk.size_mut() = (samples * 4) as u32;
            *chunk.stride_mut() = 4; // 4 bytes per sample (f32)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both process callbacks decode with a compiled-in width and rate, so
    /// the guard has to reject anything else rather than reinterpret it.
    /// Kept honest in both directions: the format we ask for must pass, or
    /// the guard would halt every working pipeline.
    #[test]
    fn only_f32le_at_the_pipeline_rate_is_decodable() {
        let cases = [
            (AudioFormat::F32LE, SAMPLE_RATE, true),
            (AudioFormat::S16LE, SAMPLE_RATE, false),
            (AudioFormat::S32LE, SAMPLE_RATE, false),
            (AudioFormat::F32BE, SAMPLE_RATE, false),
            (AudioFormat::F32LE, 44100, false),
            (AudioFormat::F32LE, 96000, false),
            (AudioFormat::S16LE, 44100, false),
        ];

        for (format, rate, decodable) in cases {
            let mut info = AudioInfoRaw::new();
            info.set_format(format);
            info.set_rate(rate);
            assert_eq!(
                unsupported_encoding(&info).is_none(),
                decodable,
                "format {} at {rate} Hz",
                format.0
            );
        }
    }

    #[test]
    fn test_audio_format_pod_creation() {
        let result = build_audio_format_pod(AudioFormat::F32LE, 48000, 2, None);
        assert!(result.is_ok(), "Should be able to create audio format pod");
        let pod_bytes = result.unwrap();
        assert!(!pod_bytes.is_empty(), "Pod should have content");

        let positioned = build_audio_format_pod(
            AudioFormat::F32LE,
            48000,
            1,
            Some(&[pipewire::spa::sys::SPA_AUDIO_CHANNEL_MONO]),
        );
        assert!(positioned.is_ok(), "Positioned pod should build");
        assert!(!positioned.unwrap().is_empty(), "Pod should have content");
    }

    /// The RT process callbacks must never grow these Vecs, and the granted
    /// channel count is only known once Format negotiation lands — after
    /// allocation, and on the other side of [`FormatState`]. So the input
    /// ring has to fit the channel ceiling unconditionally: the constructor
    /// is given no count to size itself against.
    #[test]
    fn pipeline_buffers_are_fully_reserved_up_front() {
        use crate::state::AppState;
        use std::sync::Arc;

        let processor = AudioProcessor::new(Arc::new(AppState::new()));
        let data = PipelineData::new(processor);
        assert!(
            data.input_buffer.capacity()
                >= (MAX_QUANTUM + RNNOISE_FRAME_SIZE) * MAX_INPUT_CHANNELS as usize,
            "input capacity below the channel ceiling"
        );
        assert!(
            data.output_buffer.capacity() >= OUTPUT_BUFFER_CAP + MAX_QUANTUM,
            "output capacity below the cap plus one delivery"
        );
    }

    #[test]
    fn cap_buffer_keeps_only_the_newest_samples() {
        let mut buf: Vec<f32> = (0..10).map(|i| i as f32).collect();
        cap_buffer(&mut buf, 4);
        assert_eq!(buf, [6.0, 7.0, 8.0, 9.0]);

        let mut buf = vec![1.0f32; 3];
        cap_buffer(&mut buf, 3);
        assert_eq!(buf, [1.0, 1.0, 1.0]);

        let mut buf = vec![1.0f32; 2];
        cap_buffer(&mut buf, 4);
        assert_eq!(buf, [1.0, 1.0]);
    }

    #[test]
    fn mix_to_mono_scales_and_averages() {
        let mut out = vec![0.0f32; 3];
        mix_to_mono(&[0.5, -0.25, 1.0], 1, &mut out);
        assert_eq!(out, [16384.0, -8192.0, 32768.0]);

        let mut out = vec![0.0f32; 2];
        mix_to_mono(&[1.0, 0.0, -0.5, 0.5], 2, &mut out);
        assert_eq!(out, [16384.0, 0.0]);

        // Output shorter than the input frames: extra input ignored, no panic.
        let mut out = vec![0.0f32; 1];
        mix_to_mono(&[1.0, 1.0], 1, &mut out);
        assert_eq!(out, [32768.0]);
    }

    #[test]
    fn non_finite_and_out_of_range_samples_are_coerced_into_pcm_range() {
        let cases = [
            (f32::NAN, 0.0),
            (f32::INFINITY, 0.0),
            (f32::NEG_INFINITY, 0.0),
            (1e35, 1.0),
            (-1e35, -1.0),
            (2.0, 1.0),
            (0.5, 0.5),
            (-0.5, -0.5),
            (0.0, 0.0),
        ];
        for (input, want) in cases {
            let got = sanitize_sample(input);
            assert_eq!(got, want, "sanitize_sample({input})");
        }
    }

    #[test]
    fn a_sanitized_frame_never_scales_to_infinity() {
        // The ±1.0 clamp is what keeps the RNNoise scale factor finite.
        let mut out = vec![0.0f32; 2];
        let frame: Vec<f32> = [f32::NAN, 1e35]
            .iter()
            .map(|&s| sanitize_sample(s))
            .collect();
        mix_to_mono(&frame, 1, &mut out);
        assert!(out.iter().all(|s| s.is_finite()), "{out:?}");
        assert_eq!(out, [0.0, RNNOISE_SCALE_FACTOR]);
    }

    #[test]
    fn mix_to_mono_ignores_channels_beyond_the_front_pair() {
        // 4-channel frames [FL FR RL RR]: the rear pair (e.g. an
        // interface's loopback return) must not affect the mix.
        let mut out = vec![0.0f32; 2];
        mix_to_mono(&[1.0, 0.5, -1.0, -1.0, 0.25, 0.25, 1.0, 1.0], 4, &mut out);
        assert_eq!(out, [24576.0, 8192.0]);
    }

    #[test]
    fn mix_to_mono_averages_only_the_front_pair_at_the_ceiling() {
        // 8-channel frame: channels 3..8 must not contribute.
        let mut out = vec![0.0f32; 1];
        let frame = [1.0, 0.0, 9.0, 9.0, 9.0, 9.0, 9.0, 9.0];
        mix_to_mono(&frame, 8, &mut out);
        assert_eq!(out, [16384.0]);
    }

    #[test]
    fn mix_to_mono_drops_a_trailing_partial_frame() {
        // Five samples at two channels: two whole frames, the dangling
        // sample is ignored rather than read as a half frame.
        let mut out = vec![0.0f32; 2];
        mix_to_mono(&[1.0, 1.0, 0.5, 0.5, 0.25], 2, &mut out);
        assert_eq!(out, [32768.0, 16384.0]);
    }
}
