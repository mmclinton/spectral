//! The audio thread: a PipeWire client that captures the selected input,
//! runs it through the processing chain, and feeds the daemon-owned virtual
//! source over an explicitly created link.

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
use std::time::{Duration, Instant};

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

/// Scales f32 PCM [-1.0, 1.0] into the i16-range floats [-32768.0, 32767.0]
/// RNNoise expects.
pub const RNNOISE_SCALE_FACTOR: f32 = 32768.0;

/// Maximum time to wait for the pipeline thread to finish its PipeWire setup.
pub(crate) const PIPELINE_READY_TIMEOUT: Duration = Duration::from_secs(5);

/// Backstop bound on the capture→feed buffer (1 s of mono audio). The linked
/// virtual-mic node drives its own graph section, so the feed drains
/// continuously and this should be unreachable in practice.
const OUTPUT_BUFFER_CAP: usize = SAMPLE_RATE as usize;

/// Largest per-callback delivery the buffers reserve for, in samples per
/// channel — PipeWire's `default.clock.quantum-limit`, not its
/// `default.clock.max-quantum` (2048). The limit is the ceiling PipeWire
/// allocates buffers against and clamps max-quantum to, so it is the only
/// one that bounds what a callback can actually be handed; sizing to 2048
/// would reintroduce RT-path allocations on any graph tuned above it. A
/// graph configured beyond the limit still works (the Vec grows once), but
/// every stock configuration stays allocation-free.
const MAX_QUANTUM: usize = 8192;

/// How long the virtual mic keeps running with no capture stream attached
/// before the pipeline reports and exits.
///
/// Thirty rather than twenty seconds because the two failure modes are not
/// symmetric: overshooting leaves a silent mic the user can end with
/// Disable and which the error chip is already explaining, while
/// undershooting is exactly the friction this window exists to remove. A
/// Bluetooth microphone re-pairing routinely takes longer than twenty
/// seconds, and so does a laptop resume.
const RECONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// Minimum spacing between rebuild attempts while detached. A node can
/// appear in the registry a moment before it can actually be connected, so
/// attempts have to be cheap to repeat without becoming a spin.
const REATTACH_RETRY_INTERVAL: Duration = Duration::from_millis(500);

/// Longest the pipeline loop blocks, whatever else it has scheduled. Stop
/// and every failure arrive as loop events, so this is not a tick the loop
/// depends on. It is finite because `iterate` takes a `Duration` and the
/// C API's "forever" is unreachable through it; one wakeup a second is
/// the price. Everything that cannot deliver a wake is bounded by this and
/// nothing else: a send that fails, a thread orphaned by a `start()` that
/// timed out, and a future change that records a failure from off the
/// pipeline thread, which nothing does today.
const LOOP_WAKE_BACKSTOP: Duration = Duration::from_secs(1);

/// `-ENOENT`, which PipeWire reports for a `target.object` that is not there.
const TARGET_MISSING: i32 = -2;

/// Whether a core error is the proxy half of a capture target going away,
/// rather than a failure of the connection itself.
///
/// One unplug is reported twice, in the same loop iteration: on the capture
/// stream as `StreamState::Error`, and again on that stream's proxy as a core
/// error. The crate exposes no proxy id on a `Stream`, so the two cannot be
/// tied together by identity — but `-ENOENT` says exactly "the thing you
/// named is not there", which no failure of the connection reports.
///
/// `already_detached` is load-bearing on its own: a retry against a target
/// that has not come back yet raises the same error, and calling that fatal
/// would end the reconnect window at its first attempt.
fn core_error_is_lost_target(res: i32, capture_lost: bool, already_detached: bool) -> bool {
    res == TARGET_MISSING && (capture_lost || already_detached)
}

/// What the pipeline loop should do about the capture stream this tick.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Reattach {
    /// The capture stream is live; nothing to do.
    Attached,
    /// Detached, but the target is absent or the retry interval has not
    /// elapsed.
    Waiting,
    /// Detached, the target is back, and it is time to rebuild.
    Retry,
    /// Detached past [`RECONNECT_TIMEOUT`]; report and exit.
    GiveUp,
}

/// The reconnect window's bookkeeping.
///
/// `now` is passed in rather than read here so the whole policy — the
/// deadline, the retry spacing, and what a repeated detach does to both —
/// is testable without a clock or a PipeWire graph.
#[derive(Default)]
struct ReattachState {
    detached_since: Option<Instant>,
    last_attempt: Option<Instant>,
}

impl ReattachState {
    /// The capture stream has gone away. A detach while already detached
    /// leaves the deadline alone: it runs from when audio stopped, or a
    /// device that flaps just under the timeout would never expire.
    fn detach(&mut self, now: Instant) {
        if self.detached_since.is_none() {
            self.detached_since = Some(now);
        }
        self.last_attempt = None;
    }

    /// A rebuilt capture stream is live. The window closes, and the next
    /// outage gets a full one.
    fn attach(&mut self) {
        self.detached_since = None;
        self.last_attempt = None;
    }

    fn is_detached(&self) -> bool {
        self.detached_since.is_some()
    }

    /// Records the attempt when it returns [`Reattach::Retry`], so callers
    /// cannot forget to.
    ///
    /// `attempt_in_flight` means a stream exists but has not reported live
    /// yet. Rebuilding over one destroys a connection that is merely slow:
    /// a Bluetooth headset's SCO transport is acquired lazily *because* a
    /// capture stream wants it and routinely takes seconds, so a retry every
    /// interval restarts that work from zero and the device never arrives.
    /// The deadline is checked ahead of it, so a stream that never leaves
    /// Connecting still expires on time instead of holding the window open.
    fn step(&mut self, now: Instant, target_present: bool, attempt_in_flight: bool) -> Reattach {
        let Some(since) = self.detached_since else {
            return Reattach::Attached;
        };
        if now.duration_since(since) >= RECONNECT_TIMEOUT {
            return Reattach::GiveUp;
        }
        if !target_present || attempt_in_flight {
            return Reattach::Waiting;
        }
        let due = self
            .last_attempt
            .is_none_or(|last| now.duration_since(last) >= REATTACH_RETRY_INTERVAL);
        if due {
            self.last_attempt = Some(now);
            Reattach::Retry
        } else {
            Reattach::Waiting
        }
    }

    /// How long the loop may block before this state has something timed
    /// to do: `None` while attached, otherwise the time to the give-up
    /// deadline, shortened to the next retry only when `step` would
    /// actually issue one. Takes the same inputs as `step` so the two
    /// cannot disagree: while the target is absent or an attempt is in
    /// flight, `step` withholds the retry without touching `last_attempt`,
    /// and counting the interval down anyway would hand the loop a zero
    /// timeout on every pass — a busy spin for the rest of the window.
    /// The target arriving and the stream going live are loop events, so
    /// nothing timed is missed by waiting for the deadline alone there.
    /// Waking early is harmless — `step` derives everything from `now`.
    fn wait_for(
        &self,
        now: Instant,
        target_present: bool,
        attempt_in_flight: bool,
    ) -> Option<Duration> {
        let since = self.detached_since?;
        let deadline = RECONNECT_TIMEOUT.saturating_sub(now.duration_since(since));
        if !target_present || attempt_in_flight {
            return Some(deadline);
        }
        let retry = self.last_attempt.map_or(Duration::ZERO, |last| {
            REATTACH_RETRY_INTERVAL.saturating_sub(now.duration_since(last))
        });
        Some(deadline.min(retry))
    }
}

/// `Loop::iterate` truncates its timeout to whole milliseconds, so a
/// remainder under one would become a zero timeout and spin until the
/// boundary passes. Rounding up oversleeps by less than a millisecond,
/// which `step` absorbs.
fn whole_millis_up(d: Duration) -> Duration {
    let sub = d.subsec_nanos() % 1_000_000;
    if sub == 0 {
        d
    } else {
        d + Duration::from_nanos(u64::from(1_000_000 - sub))
    }
}

/// Ready = both streams up and the feed link created; the daemon's Enable
/// blocks on this inside `PIPELINE_READY_TIMEOUT`.
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
    /// Set when a rebuilt capture stream needs the buffered input dropped:
    /// a partial frame captured at the old stride must not be decoded at
    /// the new one. Consumed on the data thread, inside the capture
    /// callback's existing borrow — both process callbacks hold that
    /// `RefCell` every cycle, so the main loop must never reach into
    /// `PipelineData` and has to hand this across as an atomic instead.
    reset_input: AtomicBool,
}

impl FormatState {
    fn new(channels: u32) -> Self {
        Self {
            channels: AtomicU32::new(channels),
            locked: AtomicBool::new(false),
            halted: AtomicBool::new(false),
            reset_input: AtomicBool::new(false),
        }
    }

    /// Let a rebuilt capture stream settle on its own channel count — a
    /// replugged device is free to offer a different one, and the granted
    /// count is what the deinterleave uses.
    ///
    /// `halted` is deliberately not cleared: every path that sets it also
    /// records into the fatal failure slot, so a halted pipeline is already
    /// on its way out and never reaches a re-attach.
    fn reset_for_reattach(&self) {
        self.locked.store(false, Ordering::SeqCst);
        self.reset_input.store(true, Ordering::SeqCst);
    }
}

/// Shared by both process callbacks through one `Rc<RefCell<..>>`: the
/// capture side appends to `output_buffer` and the feed side drains it.
///
/// That is sound only because a client context runs a single data loop,
/// which is PipeWire's default (`context.num-data-loops = 1`) and puts both
/// RT callbacks on the same thread. A user who sets it to -1 (num-cpus)
/// splits them across threads, where `RefCell`'s borrow flag is a plain
/// non-atomic `Cell` and the `Vec` mutation is a data race. Anything that
/// would let the two streams run concurrently — a second data loop, or
/// moving one stream to its own context — has to replace this with an
/// SPSC ring first, not just add a lock.
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
        // delivery. Output is trimmed to OUTPUT_BUFFER_CAP only after the
        // whole delivery is processed, so it transiently exceeds the cap by
        // the mono frames one callback can emit — and the leftover partial
        // frame carried in means that is one frame more than the delivery
        // itself: with c channels, (480c-1 + 8192c) / 480c rounds down to
        // 18 frames, not 17, at every c. The bound is
        // (MAX_QUANTUM + RNNOISE_FRAME_SIZE) - 1/c mono samples, so one
        // extra frame covers the whole channel range.
        //
        // Sized for the ceiling, not the request: the granted channel
        // count arrives only with the Format param, after allocation.
        let input_capacity = (MAX_QUANTUM + RNNOISE_FRAME_SIZE) * MAX_INPUT_CHANNELS as usize;
        let output_capacity = OUTPUT_BUFFER_CAP + MAX_QUANTUM + RNNOISE_FRAME_SIZE;
        Self {
            processor,
            input_buffer: prefaulted(input_capacity),
            output_buffer: prefaulted(output_capacity),
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
    detached: Arc<AtomicBool>,
    /// Wakes the loop so it re-reads `running`; the message itself carries
    /// nothing. The crate's channel is its cross-thread wake — a loop event
    /// source borrows the loop and cannot leave the thread.
    wake: pipewire::channel::Sender<()>,
    pipeline_thread: Option<JoinHandle<()>>,
}

/// The three signals shared between [`NativeAudioPipeline`] and its thread
/// body: the stop flag, the fatal failure slot, and whether the mic is
/// currently running on silence with no capture attached.
struct PipelineHandles {
    running: Arc<AtomicBool>,
    failure: Arc<Mutex<Option<String>>>,
    detached: Arc<AtomicBool>,
}

/// Why a granted format cannot be decoded, or `None` if it can.
///
/// Both process callbacks read 4-byte little-endian samples with a stride
/// compiled in, and RNNoise's frame is 10 ms only at [`SAMPLE_RATE`] — so an
/// encoding or rate other than the requested one cannot be reinterpreted, only
/// refused. This is the same reasoning the channel check rests on, applied
/// to the other two fields `AudioInfoRaw` carries.
///
/// `AudioFormat` has no `Debug`, hence the raw discriminant in the message;
/// it maps to `SPA_AUDIO_FORMAT_*` in `spa/param/audio/raw.h`.
pub(super) fn unsupported_encoding(info: &AudioInfoRaw) -> Option<String> {
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
        let failure = Arc::new(Mutex::new(None));
        let detached = Arc::new(AtomicBool::new(false));
        let handles = PipelineHandles {
            running: running.clone(),
            failure: failure.clone(),
            detached: detached.clone(),
        };
        let source_node = source_node.to_string();
        let virtual_name = virtual_name.to_string();
        // Ready hands back the loop's wake sender: the channel is created
        // on the pipeline thread, beside the loop it wakes, so its pipe
        // creation cannot take down this thread.
        let (ready_tx, ready_rx) = mpsc::channel::<Result<pipewire::channel::Sender<()>, String>>();

        let pipeline_thread = thread::spawn(move || {
            if let Err(e) = run_pipeline(
                processor,
                &source_node,
                &virtual_name,
                input_channels,
                &ready_tx,
                &handles,
            ) {
                record_failure(&handles.failure, format!("pipeline error: {e}"));
                let _ = ready_tx.send(Err(e.to_string()));
                eprintln!("[pipeline] pipeline error: {e}");
            }
        });

        match ready_rx.recv_timeout(PIPELINE_READY_TIMEOUT) {
            Ok(Ok(wake)) => Ok(Self {
                running,
                failure,
                detached,
                wake,
                pipeline_thread: Some(pipeline_thread),
            }),
            Ok(Err(e)) => {
                let _ = pipeline_thread.join();
                Err(format!("Pipeline setup failed: {e}").into())
            }
            Err(_) => {
                // Don't join: a wedged setup thread would hang the daemon.
                // No wake sender yet either; a thread that does reach its
                // loop reads the flag within LOOP_WAKE_BACKSTOP and exits.
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
        // Flag first, then wake, so the loop cannot drain the wake and
        // still read a stale `true`. The result is ignored: the join
        // settles the thread either way, and a wake that did fail only
        // delays it by LOOP_WAKE_BACKSTOP.
        let _ = self.wake.send(());

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

    /// Whether the capture stream is currently detached and the mic is
    /// running on silence while it waits for its device to return.
    pub fn is_detached(&self) -> bool {
        self.detached.load(Ordering::SeqCst)
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

/// A live capture stream and its listener. Tuple order is the drop order:
/// the listener unregisters its hook from a list that destroying the stream
/// would already have freed.
type CaptureStream<'c> = (
    pipewire::stream::StreamListener<Rc<RefCell<PipelineData>>>,
    StreamBox<'c>,
);

/// Everything a capture stream's callbacks hold on to, gathered so that
/// rebuilding one is a single call rather than a long argument list.
struct CaptureWiring {
    pipeline_data: Rc<RefCell<PipelineData>>,
    format_state: Arc<FormatState>,
    failure: Arc<Mutex<Option<String>>>,
    /// Set by `state_changed` when the stream errors; the loop, not the
    /// callback, tears the stream down and starts the window.
    lost: Rc<Cell<bool>>,
    /// Set when the stream reaches Paused or Streaming. Only this closes
    /// the reconnect window — a connect that returns Ok has not yet
    /// produced a working stream.
    live: Rc<Cell<bool>>,
    /// The last error the capture stream reported, for the give-up message.
    error: Rc<RefCell<Option<String>>>,
    ready: Rc<ReadyFlags>,
}

/// Build, wire and connect the capture stream against `source_node`.
///
/// Separate from [`run_pipeline`] because the reconnect window rebuilds it:
/// the virtual mic node, the feed stream and the link outlive any number of
/// these.
fn connect_capture_stream<'c>(
    core: &'c pipewire::core::Core,
    source_node: &str,
    channels: u32,
    wiring: &CaptureWiring,
) -> Result<CaptureStream<'c>, Box<dyn std::error::Error + Send + Sync>> {
    let capture_format = build_audio_format_pod(AudioFormat::F32LE, SAMPLE_RATE, channels, None)?;

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
        // makes WirePlumber relink the stream to whatever the default source now
        // is, and the virtual mic keeps denoising a microphone the user
        // never chose, with `failure` empty and the pill still Active.
        //
        // `dont-reconnect` stops the move but reports nothing: the policy
        // takes an early exit for an item that was already linked, which
        // skips the branch that would error. On its own it trades a wrong
        // mic for a silent one. `dont-fallback` is what makes an
        // unresolvable target.object a client error and a destroyed node.
        // Validated by unplugging a live capture device: it arrives as
        // `StreamState::Error("defined target not found")` in the
        // `state_changed` handler below — a stream error, not a core one,
        // so the reconnect window opens and the mic node stays up.
        "node.dont-reconnect" => "true",
        "node.dont-fallback" => "true",
    };

    let capture_stream = StreamBox::new(core, "spectral-capture", capture_props)?;

    let capture_data = wiring.pipeline_data.clone();
    let param_failure = wiring.failure.clone();
    let capture_lost = wiring.lost.clone();
    let capture_live = wiring.live.clone();
    let capture_error = wiring.error.clone();
    let capture_ready = wiring.ready.clone();
    // Each callback carries its own handle rather than reaching through the
    // listener's user data, which is what keeps the main-loop handler off
    // the RT callback's `RefCell`. See [`FormatState`].
    let param_format = wiring.format_state.clone();
    let capture_format_state = wiring.format_state.clone();
    let capture_listener = capture_stream
        .add_local_listener_with_user_data(capture_data)
        .state_changed(move |_stream, _data, _old, new| match new {
            // The one recoverable failure this pipeline has: the capture
            // stream is the only object it can rebuild in place, so the
            // loop owns the deadline and the give-up message rather than
            // this being written straight into the fatal slot.
            StreamState::Error(msg) => {
                *capture_error.borrow_mut() = Some(msg);
                capture_lost.set(true);
            }
            StreamState::Paused | StreamState::Streaming => {
                capture_ready.capture.set(true);
                capture_live.set(true);
            }
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

    Ok((capture_listener, capture_stream))
}

fn run_pipeline(
    processor: AudioProcessor,
    source_node: &str,
    virtual_name: &str,
    input_channels: u32,
    ready_tx: &mpsc::Sender<Result<pipewire::channel::Sender<()>, String>>,
    handles: &PipelineHandles,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let PipelineHandles {
        running,
        failure,
        detached,
    } = handles;

    pipewire::init();

    let mainloop = MainLoopBox::new(None)?;
    // Arrival alone is the point: `iterate` returns and the loop condition
    // re-reads `running`.
    let (wake, wake_rx) = pipewire::channel::channel::<()>();
    let _wake = wake_rx.attach(mainloop.loop_(), |()| {});
    let context = ContextBox::new(mainloop.loop_(), None)?;
    let core = context.connect(Some(super::manager_client_properties()))?;

    let channels = input_channels.clamp(1, MAX_INPUT_CHANNELS);

    let pipeline_data = Rc::new(RefCell::new(PipelineData::new(processor)));
    let format_state = Arc::new(FormatState::new(channels));

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
    let core_error: Rc<RefCell<Option<(u32, i32, String)>>> = Rc::new(RefCell::new(None));

    let wiring = CaptureWiring {
        pipeline_data: pipeline_data.clone(),
        format_state: format_state.clone(),
        failure: failure.clone(),
        lost: Rc::new(Cell::new(false)),
        live: Rc::new(Cell::new(false)),
        error: Rc::new(RefCell::new(None)),
        ready: ready.clone(),
    };
    let mut reattach = ReattachState::default();
    let mut capture = Some(connect_capture_stream(
        &core,
        source_node,
        channels,
        &wiring,
    )?);

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

    // Parked rather than judged here: whether a proxy error is fatal depends
    // on what the capture stream reported in the same iteration, which this
    // callback cannot see. The loop classifies it. First error wins, matching
    // `record_failure` — later ones are usually cascades of the first.
    let core_error_slot = core_error.clone();
    let _core_listener = core
        .add_listener_local()
        .error(move |id, _seq, res, message| {
            let mut slot = core_error_slot.borrow_mut();
            if slot.is_none() {
                *slot = Some((id, res, message.to_string()));
            }
        })
        .register();

    // The session manager configures the mic node and the feed stream
    // asynchronously; watch the registry for their ports, then link them.
    // The port list exists only to create that one link: once it is up,
    // collection stops and the list is dropped — otherwise it would grow
    // for the pipeline's lifetime as devices hot-plug.
    let mic_node_id: Rc<Cell<Option<u32>>> = Rc::new(Cell::new(None));
    // The capture target's own global. Tracked by id so its removal can be
    // seen: a name match alone could only ever say "it appeared once".
    let target_node_id: Rc<Cell<Option<u32>>> = Rc::new(Cell::new(None));
    let ports: Rc<RefCell<Vec<(u32, u32, String)>>> = Rc::new(RefCell::new(Vec::new()));
    let link_established = Rc::new(Cell::new(false));

    let registry = core.get_registry()?;
    let mic_node_slot = mic_node_id.clone();
    let target_slot = target_node_id.clone();
    let target_removed_slot = target_node_id.clone();
    let ports_slot = ports.clone();
    let link_established_slot = link_established.clone();
    let mic_node_name = virtual_name.to_string();
    let target_name = source_node.to_string();
    let _registry_listener = registry
        .add_listener_local()
        .global(move |global| {
            let Some(props) = &global.props else { return };
            match global.type_ {
                ObjectType::Node if props.get("node.name") == Some(mic_node_name.as_str()) => {
                    mic_node_slot.set(Some(global.id));
                }
                ObjectType::Node if props.get("node.name") == Some(target_name.as_str()) => {
                    target_slot.set(Some(global.id));
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
        .global_remove(move |id| {
            if target_removed_slot.get() == Some(id) {
                target_removed_slot.set(None);
            }
        })
        .register();

    let mut feed_link: Option<Link> = None;
    let mut ready_sent = false;

    while running.load(Ordering::SeqCst) {
        // Blocks until an event: stop and every failure arrive as one, so
        // the only timed work is the reconnect window's own schedule. The
        // backstop caps that schedule rather than merely standing in for it,
        // because the window's next deadline can be the whole RECONNECT_TIMEOUT
        // away and a loop nobody can wake has to notice `running` regardless.
        let timeout = reattach
            .wait_for(
                Instant::now(),
                target_node_id.get().is_some(),
                capture.is_some(),
            )
            .map_or(LOOP_WAKE_BACKSTOP, |wait| wait.min(LOOP_WAKE_BACKSTOP));
        let res = mainloop.loop_().iterate(whole_millis_up(timeout));
        if res < 0 {
            let errno = -res;
            // A signal or a ptrace attach interrupts the wait without
            // meaning anything about the graph; PipeWire's own run loop
            // retries the same way.
            if std::io::Error::from_raw_os_error(errno).kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            record_failure(failure, format!("main loop iterate failed: errno {errno}"));
            break;
        }
        // The stop wake arrives as an ordinary iteration, so without this the
        // body below still runs once: a due retry would build and connect a
        // capture stream the teardown two statements later throws away, and
        // that server round trip lands inside a join the daemon performs
        // while holding the virtual-mic lock.
        if !running.load(Ordering::SeqCst) {
            break;
        }
        // Classified here rather than in the callback, which cannot see what
        // the capture stream reported in the same iteration. Read `lost`
        // without consuming it — the teardown below owns that.
        if let Some((id, res, message)) = core_error.borrow_mut().take() {
            if core_error_is_lost_target(res, wiring.lost.get(), reattach.is_detached()) {
                eprintln!("[pipeline] capture target gone (proxy reported {res} on object {id})");
            } else {
                record_failure(
                    failure,
                    format!("PipeWire error on object {id}: {res} {message}"),
                );
            }
        }

        if lock_or_recover(failure).is_some() {
            break;
        }

        let now = Instant::now();

        // Tear the stream down here rather than in the callback, which must
        // not drop the object it was invoked from. Unconditionally, even
        // while already detached: a retry whose stream errors would
        // otherwise stay in `capture`, where `step` reads it as an attempt
        // still in flight and withholds every further retry — so the window
        // would run out with the device present and nothing trying to reach
        // it.
        if wiring.lost.replace(false) {
            capture = None;
            wiring.live.set(false);
            let why = wiring.error.borrow().clone().unwrap_or_default();
            if !ready_sent {
                // Nothing is holding the mic yet — Enable is still blocked on
                // the ready handshake — so a window here protects no audio and
                // costs the caller the whole ready timeout, after which it
                // reports a generic timeout instead of this reason.
                record_failure(failure, format!("capture stream error: {why}"));
                break;
            }
            if !reattach.is_detached() {
                reattach.detach(now);
                detached.store(true, Ordering::SeqCst);
                eprintln!("[pipeline] capture target lost, holding the mic open: {why}");
            }
        }

        // Only an observed-live stream closes the window. Treating a
        // successful connect as success would give a device that errors
        // immediately after connecting an unlimited number of full windows.
        if reattach.is_detached() && capture.is_some() && wiring.live.get() {
            reattach.attach();
            detached.store(false, Ordering::SeqCst);
            eprintln!("[pipeline] capture target re-attached");
        }

        match reattach.step(now, target_node_id.get().is_some(), capture.is_some()) {
            Reattach::Attached | Reattach::Waiting => {}
            Reattach::Retry => {
                // No stream to drop: `step` withholds Retry while one is in
                // flight, and the teardown above is what cleared this slot.
                wiring.format_state.reset_for_reattach();
                match connect_capture_stream(&core, source_node, channels, &wiring) {
                    Ok(stream) => capture = Some(stream),
                    Err(e) => eprintln!("[pipeline] capture re-attach failed: {e}"),
                }
            }
            Reattach::GiveUp => {
                let secs = RECONNECT_TIMEOUT.as_secs();
                let why = wiring.error.borrow().clone().unwrap_or_default();
                eprintln!("[pipeline] giving up on the capture target after {secs}s: {why}");
                record_failure(
                    failure,
                    format!("the input device did not return within {secs} seconds"),
                );
                break;
            }
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
            let _ = ready_tx.send(Ok(wake.clone()));
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

            // An untrusted format: drop the delivery rather than
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

            // The rebuilt stream's first delivery. Done here rather than on
            // the main loop because the feed callback holds this `RefCell`
            // every cycle, and a main-loop `borrow_mut` would race it.
            if format.reset_input.swap(false, Ordering::SeqCst) {
                input_buffer.clear();
                processor.reset_for_reattach();
            }

            // `as_chunks` hands back `[u8; 4]` rather than a slice, so the
            // four bounds checks per sample that indexing needs are gone
            // from the capture callback. The trailing partial frame is
            // discarded either way.
            let (frames, _) = valid_data.as_chunks::<4>();
            for frame in frames {
                input_buffer.push(sanitize_sample(f32::from_le_bytes(*frame)));
            }

            // Process complete frames (480 samples per channel) in the
            // preallocated scratch buffers — the RT thread must not
            // allocate. Frames advance by offset with one drain at the end:
            // draining per frame would shift the whole remainder each
            // iteration, quadratic in the number of buffered frames.
            let frame_size = RNNOISE_FRAME_SIZE * channels;
            let mut consumed = 0;
            while input_buffer.len() - consumed >= frame_size {
                mix_to_mono(
                    &input_buffer[consumed..consumed + frame_size],
                    channels,
                    mono_scratch,
                );
                processor.process_frame(mono_scratch, processed_scratch);

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

/// An empty `Vec` whose whole capacity is already resident.
///
/// Both rings are larger than glibc's mmap threshold, so a bare
/// `with_capacity` returns pages the kernel maps on first write — and the
/// first write is the RT callback's, one minor fault per page across the
/// first second of audio. Writing every slot here moves that onto the
/// pipeline thread. The fill value is non-zero on purpose: a zero fill
/// lowers to `calloc`, whose fresh mmap pages are likewise left untouched.
fn prefaulted(capacity: usize) -> Vec<f32> {
    let mut buf = vec![f32::NAN; capacity];
    buf.clear();
    buf
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

            // PipeWire reads nothing from the buffer until the chunk
            // describes it.
            let chunk = datas[0].chunk_mut();
            *chunk.offset_mut() = 0;
            *chunk.size_mut() = (samples * 4) as u32;
            *chunk.stride_mut() = 4;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(base: Instant, secs: u64) -> Instant {
        base + Duration::from_secs(secs)
    }

    #[test]
    fn a_missing_target_the_capture_stream_also_reported_is_not_fatal() {
        assert!(core_error_is_lost_target(TARGET_MISSING, true, false));
    }

    #[test]
    fn a_missing_target_during_an_open_window_is_not_fatal() {
        // A retry against a device that has not come back yet raises the same
        // error; calling it fatal would end the window at the first attempt.
        assert!(core_error_is_lost_target(TARGET_MISSING, false, true));
    }

    #[test]
    fn a_missing_target_nobody_else_reported_stays_fatal() {
        assert!(!core_error_is_lost_target(TARGET_MISSING, false, false));
    }

    #[test]
    fn a_connection_failure_stays_fatal_even_mid_window() {
        // -EPIPE: the connection itself is gone, which no re-attach repairs.
        assert!(!core_error_is_lost_target(-32, true, true));
    }

    #[test]
    fn a_state_that_never_detached_reports_attached() {
        let mut state = ReattachState::default();
        assert_eq!(state.step(Instant::now(), true, false), Reattach::Attached);
        assert!(!state.is_detached());
    }

    #[test]
    fn a_detached_state_waits_while_the_target_is_absent() {
        let base = Instant::now();
        let mut state = ReattachState::default();
        state.detach(base);
        assert_eq!(state.step(at(base, 1), false, false), Reattach::Waiting);
        assert_eq!(state.step(at(base, 5), false, false), Reattach::Waiting);
    }

    #[test]
    fn a_present_target_is_retried_once_per_interval() {
        let base = Instant::now();
        let mut state = ReattachState::default();
        state.detach(base);
        assert_eq!(state.step(base, true, false), Reattach::Retry);
        // Too soon: one attempt per interval, so a target that appears
        // before it can be connected costs retries and not a spin.
        assert_eq!(
            state.step(base + Duration::from_millis(100), true, false),
            Reattach::Waiting
        );
        assert_eq!(
            state.step(base + REATTACH_RETRY_INTERVAL, true, false),
            Reattach::Retry
        );
    }

    /// The retry is for a target with no stream on it. A stream that is
    /// still negotiating gets the interval back rather than being replaced,
    /// or a device whose transport takes longer than one interval to come up
    /// is restarted from zero every time and never finishes.
    #[test]
    fn an_attempt_still_in_flight_is_left_alone_rather_than_replaced() {
        let base = Instant::now();
        let mut state = ReattachState::default();
        state.detach(base);
        assert_eq!(state.step(base, true, false), Reattach::Retry);
        for elapsed in [1, 2, 5, 10] {
            assert_eq!(
                state.step(at(base, elapsed), true, true),
                Reattach::Waiting,
                "an in-flight attempt was preempted after {elapsed}s"
            );
        }
        // The attempt died without ever going live: the slot is free, and
        // the throttle is the only thing left holding the next rebuild.
        assert_eq!(state.step(at(base, 10), true, false), Reattach::Retry);
    }

    /// The other direction of the same guard: waiting on an attempt must not
    /// become waiting forever, so the deadline is checked ahead of it.
    #[test]
    fn a_stream_stuck_negotiating_still_gives_up_at_the_deadline() {
        let base = Instant::now();
        let mut state = ReattachState::default();
        state.detach(base);
        assert_eq!(
            state.step(
                base + RECONNECT_TIMEOUT - Duration::from_millis(1),
                true,
                true
            ),
            Reattach::Waiting
        );
        assert_eq!(
            state.step(base + RECONNECT_TIMEOUT, true, true),
            Reattach::GiveUp
        );
    }

    #[test]
    fn the_window_expires_at_the_timeout() {
        let base = Instant::now();
        let mut state = ReattachState::default();
        state.detach(base);
        assert_eq!(
            state.step(
                base + RECONNECT_TIMEOUT - Duration::from_millis(1),
                false,
                false
            ),
            Reattach::Waiting
        );
        assert_eq!(
            state.step(base + RECONNECT_TIMEOUT, false, false),
            Reattach::GiveUp
        );
    }

    #[test]
    fn a_second_detach_does_not_extend_the_window() {
        let base = Instant::now();
        let mut state = ReattachState::default();
        state.detach(base);
        // A failed re-attach detaches again. The deadline runs from when
        // audio actually stopped, or a flapping device would never expire.
        state.detach(at(base, 20));
        assert_eq!(state.step(at(base, 31), false, false), Reattach::GiveUp);
    }

    #[test]
    fn a_successful_attach_gives_the_next_outage_a_full_window() {
        let base = Instant::now();
        let mut state = ReattachState::default();
        state.detach(base);
        state.attach();
        assert!(!state.is_detached());
        state.detach(at(base, 100));
        assert_eq!(state.step(at(base, 120), false, false), Reattach::Waiting);
        assert_eq!(state.step(at(base, 130), false, false), Reattach::GiveUp);
    }

    /// The loop blocks for as long as the state machine has nothing timed
    /// to do, so an attached pipeline must not name a deadline at all.
    #[test]
    fn an_attached_state_has_no_deadline() {
        let mut state = ReattachState::default();
        assert_eq!(state.wait_for(Instant::now(), true, false), None);
        state.detach(Instant::now());
        state.attach();
        assert_eq!(state.wait_for(Instant::now(), true, true), None);
    }

    /// While a retry is possible the wait is whichever comes first: the
    /// next retry or the give-up deadline. Waking early is harmless,
    /// sleeping past either is not.
    #[test]
    fn a_retryable_state_wakes_for_the_earlier_of_retry_and_deadline() {
        let base = Instant::now();
        let mut state = ReattachState::default();
        state.detach(base);

        // Target present, no attempt yet: `step` retries now, so no wait.
        assert_eq!(state.wait_for(base, true, false), Some(Duration::ZERO));

        // The attempt failed outright (no stream left): the throttle is
        // all that holds the next one, so only the interval's remainder.
        assert_eq!(state.step(base, true, false), Reattach::Retry);
        assert_eq!(
            state.wait_for(base + Duration::from_millis(100), true, false),
            Some(REATTACH_RETRY_INTERVAL - Duration::from_millis(100))
        );
        assert_eq!(
            state.wait_for(base + REATTACH_RETRY_INTERVAL, true, false),
            Some(Duration::ZERO)
        );

        // Near the deadline the window remainder is shorter than a fresh
        // retry interval, so the deadline wins.
        let late = base + RECONNECT_TIMEOUT - Duration::from_millis(50);
        assert_eq!(
            state.step(late - Duration::from_millis(10), true, false),
            Reattach::Retry
        );
        assert_eq!(
            state.wait_for(late, true, false),
            Some(Duration::from_millis(50))
        );

        // Past the deadline: due now, so `step` can report GiveUp.
        assert_eq!(
            state.wait_for(at(base, 31), true, false),
            Some(Duration::ZERO)
        );
    }

    /// `step` withholds the retry while the target is absent or an attempt
    /// is still negotiating, and leaves `last_attempt` alone while it does.
    /// Counting the interval down regardless would hand the loop a zero
    /// timeout every pass — a busy spin for the rest of the window. Only
    /// the deadline is timed there; the target arriving and the stream
    /// going live are events.
    #[test]
    fn a_withheld_retry_waits_for_the_deadline_not_the_interval() {
        let base = Instant::now();
        let mut state = ReattachState::default();
        state.detach(base);

        // Target absent, nothing tried: only the window is timed.
        assert_eq!(state.wait_for(base, false, false), Some(RECONNECT_TIMEOUT));

        // An attempt that is still connecting well past the interval.
        assert_eq!(state.step(base, true, false), Reattach::Retry);
        for secs in [1, 2, 5, 10] {
            assert_eq!(
                state.wait_for(at(base, secs), true, true),
                Some(RECONNECT_TIMEOUT - Duration::from_secs(secs)),
                "an in-flight attempt scheduled a wake after {secs}s"
            );
        }

        // The target vanished again after that attempt: same rule.
        assert_eq!(
            state.wait_for(at(base, 5), false, false),
            Some(RECONNECT_TIMEOUT - Duration::from_secs(5))
        );
    }

    /// `Loop::iterate` truncates to whole milliseconds, so a sub-millisecond
    /// remainder must round up rather than become a zero-timeout spin.
    #[test]
    fn a_sub_millisecond_wait_rounds_up_to_one() {
        let cases = [
            (Duration::ZERO, Duration::ZERO),
            (Duration::from_micros(700), Duration::from_millis(1)),
            (Duration::from_millis(50), Duration::from_millis(50)),
            (Duration::from_micros(499_300), Duration::from_millis(500)),
            (Duration::from_nanos(1_000_001), Duration::from_millis(2)),
        ];
        for (input, expected) in cases {
            assert_eq!(whole_millis_up(input), expected, "{input:?}");
        }
    }

    /// Both process callbacks decode with a compiled-in width and rate, so
    /// the guard has to reject anything else rather than reinterpret it.
    /// Kept honest in both directions: the requested format must pass, or
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
    fn format_pods_build_with_and_without_channel_positions() {
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
            data.output_buffer.capacity() >= OUTPUT_BUFFER_CAP + MAX_QUANTUM + RNNOISE_FRAME_SIZE,
            "output capacity below the cap plus one delivery and its carried frame"
        );
        assert!(
            data.input_buffer.is_empty() && data.output_buffer.is_empty(),
            "pre-faulting must leave the rings empty, not full of fill values"
        );
    }

    /// The reservation has to cover the worst callback at every channel
    /// count, not just the arithmetic that looks obvious at one. Entry
    /// length is the cap (the previous callback trimmed to it), the leftover
    /// is one sample short of a whole frame, and the delivery is maximal.
    #[test]
    fn no_channel_count_can_grow_the_output_buffer_on_the_rt_path() {
        use crate::state::AppState;
        use std::sync::Arc;

        let reserved = PipelineData::new(AudioProcessor::new(Arc::new(AppState::new())))
            .output_buffer
            .capacity();

        for channels in 1..=MAX_INPUT_CHANNELS as usize {
            let frame_size = RNNOISE_FRAME_SIZE * channels;
            let buffered = (frame_size - 1) + MAX_QUANTUM * channels;
            let peak = OUTPUT_BUFFER_CAP + (buffered / frame_size) * RNNOISE_FRAME_SIZE;
            assert!(
                peak <= reserved,
                "at {channels} channels the peak is {peak} samples, past the reserved {reserved}"
            );
        }
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
