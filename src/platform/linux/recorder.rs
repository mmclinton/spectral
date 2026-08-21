//! Native PipeWire WAV recorder and player for the voice-test A/B path.
//!
//! Two self-contained types that mirror `NativeAudioPipeline`'s proven
//! lifecycle: a dedicated mainloop thread per subsystem, an `mpsc`
//! ready-handshake with [`super::pipewire::PIPELINE_READY_TIMEOUT`], a
//! `running: Arc<AtomicBool>` flag, a `failure: Arc<Mutex<Option<String>>>`
//! slot (first error wins), and a `Drop` that calls `stop()`. Replaces the
//! former `pw-record`/`pw-play` shell-out so the daemon no longer depends on
//! the PipeWire *tools* package at runtime — only the libraries it links.
//!
//! WAV encoding/decoding uses `hound` (pure Rust, already transitively in
//! `Cargo.lock` via `nnnoiseless`); see <https://docs.rs/hound>.
//!
//! Both files are mono 48 kHz 32-bit float (matching `pw-record --channels 1
//! --format f32`). Recording captures the raw f32 the graph delivers — no
//! i16 scaling, so the file is bit-identical to what `pw-record` produced.

use super::pipewire::{
    PIPELINE_READY_TIMEOUT, SAMPLE_RATE, build_audio_format_pod, unsupported_encoding,
};
use crate::state::lock_or_recover;
use pipewire::spa::{
    param::ParamType,
    param::audio::{AudioFormat, AudioInfoRaw},
    param::format::{MediaSubtype, MediaType},
    param::format_utils,
    pod::Pod,
    utils::Direction,
};
use pipewire::{
    context::ContextBox,
    main_loop::MainLoopBox,
    properties::properties,
    stream::{StreamBox, StreamFlags, StreamState},
};
use std::cell::{Cell, RefCell};
use std::fs::{File, OpenOptions};
use std::io::BufWriter;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// PipeWire's mono channel position.
const MONO: u32 = pipewire::spa::sys::SPA_AUDIO_CHANNEL_MONO;

/// Set while a recorder thread owns the two WAV paths, cleared by that
/// thread on its way out — after both writers are finalized.
///
/// A session whose ready handshake times out is abandoned rather than
/// joined, because joining a wedged setup would hang the daemon (the same
/// call the pipeline makes). But it opened both writers before the
/// handshake, so it still holds them and will finalize them, and finalize
/// seeks to offset 0 to rewrite the header. The paths are fixed, so
/// without this gate the next session could truncate and reopen them
/// first and take that header write into the middle of its own recording.
static RECORDER_SESSION_ACTIVE: AtomicBool = AtomicBool::new(false);

/// How long [`NativeRecorder::start`] waits for an abandoned session to
/// release the WAV paths. Bounded on purpose: a wedged session must fail
/// the new recording, never block the daemon behind it.
const SESSION_RELEASE_TIMEOUT: Duration = Duration::from_millis(500);

/// WAV spec shared by recorder and player: mono, 48 kHz, 32-bit float.
const WAV_SPEC: hound::WavSpec = hound::WavSpec {
    channels: 1,
    sample_rate: SAMPLE_RATE,
    bits_per_sample: 32,
    sample_format: hound::SampleFormat::Float,
};

/// How many captured samples between header-checkpoint flushes. ~1 s of
/// mono 48 kHz audio; bounds the post-SIGKILL data loss to the trailing
/// second at most, since hound's reader trusts the data-size header.
const FLUSH_INTERVAL_SAMPLES: u64 = SAMPLE_RATE as u64;

/// Hard daemon-side cap on one recording: 60 s per stream. The GUI stops
/// recordings at 15 s, so this backstop only fires when no client is left
/// to send StopRecording (e.g. the GUI died mid-recording) — bounding disk
/// use instead of writing until the disk is full. Reaching it goes through
/// the failure slot so the GetState reap tears the recorder down and
/// reports why.
const MAX_RECORDING_SAMPLES: u64 = 60 * SAMPLE_RATE as u64;

/// Check a granted Format against the mono/48 kHz/F32LE all three of this
/// module's streams assume, recording the mismatch in `failure` if any.
///
/// Every stream here offers a fixed pod, but an offer is not a guarantee.
/// Both capture callbacks decode at a hardcoded 4-byte stride into a
/// `WavWriter` whose header is fixed by [`WAV_SPEC`], and the player writes
/// at the same fixed stride — so a grant that differs would be
/// reinterpreted at the wrong width rather than refused, and the voice test
/// would compare two clips that are silently wrong (a stereo grant plays
/// back at double speed) with nothing in the failure slot to say so. The
/// pipeline guards both of its streams for exactly this reason, including
/// the mono output side; the recorder simply predates that work.
fn verify_granted_format(
    id: u32,
    param: Option<&Pod>,
    failure: &Arc<Mutex<Option<String>>>,
    stream: &str,
) {
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
    } else if info.channels() != 1 {
        Some(format!(
            "{} channels is not the mono the WAV holds",
            info.channels()
        ))
    } else {
        None
    };
    if let Some(why) = why {
        record_failure(failure, format!("{stream}: {why}"));
    }
}

/// First error wins: later failures are usually cascades of the first.
fn record_failure(slot: &Mutex<Option<String>>, msg: String) {
    let mut guard = lock_or_recover(slot);
    if guard.is_none() {
        *guard = Some(msg);
    }
}

/// Which side a [`NativePlayer`] is playing back. Playback is mutually
/// exclusive in the daemon, so a single slot carries this tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackKind {
    /// The raw (pre-processing) capture.
    Original,
    /// The processed (post-mic) capture.
    Filtered,
}

// ---------------------------------------------------------------------------
// NativeRecorder
// ---------------------------------------------------------------------------

/// Per-stream state carried inside the recorder's PipeWire callbacks.
struct CaptureSlot {
    writer: Option<hound::WavWriter<BufWriter<File>>>,
    samples_since_flush: u64,
    samples_written: u64,
    /// Set by the capture callback when a sample fails to write, and drained
    /// by the recorder loop, which is the only side that can reach the
    /// failure slot. The callback stops writing the delivery it failed on;
    /// later deliveries still try, and simply overwrite this with their own
    /// cause until the loop drains it a few milliseconds later. Harmless —
    /// `BufWriter` keeps whatever it could not flush — and not worth a guard
    /// on the audio path.
    write_error: Option<String>,
}

/// A write failure parked by either capture callback, cleared as it is read;
/// the raw slot wins when both hold one. Callbacks only run inside
/// `iterate()`, so the two borrows here can never conflict with one.
fn take_write_error(
    raw: &Rc<RefCell<CaptureSlot>>,
    filtered: &Rc<RefCell<CaptureSlot>>,
) -> Option<String> {
    raw.borrow_mut()
        .write_error
        .take()
        .or_else(|| filtered.borrow_mut().write_error.take())
}

/// Handle to the recorder thread owning both capture streams (raw source +
/// virtual-mic node) on a single PipeWire mainloop.
pub struct NativeRecorder {
    running: Arc<AtomicBool>,
    failure: Arc<Mutex<Option<String>>>,
    thread: Option<JoinHandle<()>>,
}

impl NativeRecorder {
    /// Start capturing `source_node` (raw) and `mic_name` (filtered) into the
    /// two WAV paths. Blocks on the ready handshake; a stream that fails to
    /// come up surfaces as an `Err` here.
    pub fn start(
        source_node: &str,
        mic_name: &str,
        original: PathBuf,
        filtered: PathBuf,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        claim_session()?;

        let running = Arc::new(AtomicBool::new(true));
        let running_clone = running.clone();
        let failure = Arc::new(Mutex::new(None));
        let failure_clone = failure.clone();
        let source_node = source_node.to_string();
        let mic_name = mic_name.to_string();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();

        let thread = thread::spawn(move || {
            if let Err(e) = run_recorder(
                &source_node,
                &mic_name,
                original,
                filtered,
                running_clone,
                &ready_tx,
                &failure_clone,
            ) {
                record_failure(&failure_clone, format!("recorder error: {e}"));
                let _ = ready_tx.send(Err(e.to_string()));
                eprintln!("[NativeRecorder] error: {e}");
            }
            // Last thing this thread does: run_recorder has finalized both
            // writers by now, so the paths are free for the next session.
            RECORDER_SESSION_ACTIVE.store(false, Ordering::SeqCst);
        });

        match ready_rx.recv_timeout(PIPELINE_READY_TIMEOUT) {
            Ok(Ok(())) => Ok(Self {
                running,
                failure,
                thread: Some(thread),
            }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(format!("Recorder setup failed: {e}").into())
            }
            Err(_) => {
                // Don't join: a wedged setup thread would hang the daemon.
                // The session flag stays set until it finishes finalizing,
                // which is what keeps the next Record off these paths.
                running.store(false, Ordering::SeqCst);
                Err(
                    format!("Recorder did not become ready within {PIPELINE_READY_TIMEOUT:?}")
                        .into(),
                )
            }
        }
    }

    /// Signal the recorder thread to quit and join it; finalizes both WAVs
    /// so any write error is observable. A second call is a no-op.
    pub fn stop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        if let Some(handle) = self.thread.take()
            && handle.join().is_err()
        {
            eprintln!("Recorder thread panicked during shutdown");
        }
    }

    /// Why the recorder died, if it has. A finished thread with an empty
    /// failure slot (e.g. a panic) still counts while `running` is set.
    pub fn failure(&self) -> Option<String> {
        if let Some(msg) = lock_or_recover(&self.failure).clone() {
            return Some(msg);
        }
        let thread_exited = self.thread.as_ref().is_some_and(JoinHandle::is_finished);
        (self.running.load(Ordering::SeqCst) && thread_exited)
            .then(|| "recorder thread exited unexpectedly".to_string())
    }
}

impl Drop for NativeRecorder {
    fn drop(&mut self) {
        if self.thread.is_some() {
            self.stop();
        }
    }
}

/// Take ownership of the two WAV paths for a new recorder session, waiting
/// out an abandoned predecessor that is still finalizing.
///
/// The daemon serializes Record requests, so the only contender is a
/// timed-out session; the wait is short and the failure is explicit rather
/// than a silently corrupted pair of clips.
fn claim_session() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let deadline = std::time::Instant::now() + SESSION_RELEASE_TIMEOUT;
    loop {
        if RECORDER_SESSION_ACTIVE
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err("a previous voice-test recording is still shutting down".into());
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[derive(Default)]
struct RecorderReadyFlags {
    raw: Cell<bool>,
    filtered: Cell<bool>,
}

fn run_recorder(
    source_node: &str,
    mic_name: &str,
    original: PathBuf,
    filtered: PathBuf,
    running: Arc<AtomicBool>,
    ready_tx: &mpsc::Sender<Result<(), String>>,
    failure: &Arc<Mutex<Option<String>>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    pipewire::init();

    let mainloop = MainLoopBox::new(None)?;
    let context = ContextBox::new(mainloop.loop_(), None)?;
    let core = context.connect(None)?;

    let ready = Rc::new(RecorderReadyFlags::default());
    let format_pod = build_audio_format_pod(AudioFormat::F32LE, SAMPLE_RATE, 1, Some(&[MONO]))?;

    // Surface any core/proxy error the streams' own state_changed handlers
    // do not see. An unresolvable target.object is not one of them: on the
    // pipeline's identical setup that was observed arriving as a stream
    // error, so both routes reach the same failure slot and neither is
    // load-bearing alone.
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

    // --- Raw capture stream (the source device) ---------------------------
    let raw_slot = Rc::new(RefCell::new(CaptureSlot {
        writer: Some(open_wav_writer(&original)?),
        samples_since_flush: 0,
        samples_written: 0,
        write_error: None,
    }));
    let raw_props = properties! {
        *pipewire::keys::MEDIA_TYPE => "Audio",
        *pipewire::keys::MEDIA_CATEGORY => "Capture",
        *pipewire::keys::MEDIA_ROLE => "Communication",
        *pipewire::keys::NODE_NAME => "spectral-recorder-raw",
        *pipewire::keys::NODE_DESCRIPTION => "Spectral Voice-Test Recorder",
        "target.object" => source_node,
        // dont-reconnect stops WirePlumber silently moving the stream to
        // the default source once it has been linked; dont-fallback is what
        // makes an unresolvable target an error rather than a wrong-node
        // recording. Neither covers the other's case — see the pair in
        // `pipewire.rs` for which policy branch each one turns.
        "node.dont-reconnect" => "true",
        "node.dont-fallback" => "true",
    };
    let raw_stream = StreamBox::new(&core, "spectral-recorder-raw", raw_props)?;
    let raw_slot_cb = raw_slot.clone();
    let raw_failure = failure.clone();
    let raw_ready = ready.clone();
    let _raw_listener = raw_stream
        .add_local_listener_with_user_data(raw_slot_cb)
        .state_changed(move |_stream, _data, _old, new| match new {
            StreamState::Error(msg) => {
                record_failure(&raw_failure, format!("raw stream error: {msg}"));
            }
            StreamState::Paused | StreamState::Streaming => raw_ready.raw.set(true),
            _ => {}
        })
        .param_changed({
            let failure = failure.clone();
            move |_stream, _data, id, param| {
                verify_granted_format(id, param, &failure, "raw stream");
            }
        })
        .process(process_capture)
        .register()?;
    // AUTOCONNECT | MAP_BUFFERS only — no RT_PROCESS. The process callback
    // does buffered file I/O (not RT-safe); pw-cat itself sets no RT_PROCESS
    // and does its sndfile I/O on the main loop.
    raw_stream.connect(
        Direction::Input,
        None,
        StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS,
        &mut [Pod::from_bytes(&format_pod).ok_or("Failed to build raw format pod")?],
    )?;

    // --- Filtered capture stream (the virtual mic node) -------------------
    let filtered_slot = Rc::new(RefCell::new(CaptureSlot {
        writer: Some(open_wav_writer(&filtered)?),
        samples_since_flush: 0,
        samples_written: 0,
        write_error: None,
    }));
    let filtered_props = properties! {
        *pipewire::keys::MEDIA_TYPE => "Audio",
        *pipewire::keys::MEDIA_CATEGORY => "Capture",
        *pipewire::keys::MEDIA_ROLE => "Communication",
        *pipewire::keys::NODE_NAME => "spectral-recorder-filtered",
        *pipewire::keys::NODE_DESCRIPTION => "Spectral Voice-Test Recorder",
        "target.object" => mic_name,
        "node.dont-reconnect" => "true",
        "node.dont-fallback" => "true",
    };
    let filtered_stream = StreamBox::new(&core, "spectral-recorder-filtered", filtered_props)?;
    let filtered_slot_cb = filtered_slot.clone();
    let filtered_failure = failure.clone();
    let filtered_ready = ready.clone();
    let _filtered_listener = filtered_stream
        .add_local_listener_with_user_data(filtered_slot_cb)
        .state_changed(move |_stream, _data, _old, new| match new {
            StreamState::Error(msg) => {
                record_failure(&filtered_failure, format!("filtered stream error: {msg}"));
            }
            StreamState::Paused | StreamState::Streaming => filtered_ready.filtered.set(true),
            _ => {}
        })
        .param_changed({
            let failure = failure.clone();
            move |_stream, _data, id, param| {
                verify_granted_format(id, param, &failure, "filtered stream");
            }
        })
        .process(process_capture)
        .register()?;
    filtered_stream.connect(
        Direction::Input,
        None,
        StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS,
        &mut [Pod::from_bytes(&format_pod).ok_or("Failed to build filtered format pod")?],
    )?;

    let mut ready_sent = false;
    while running.load(Ordering::SeqCst) {
        let res = mainloop.loop_().iterate(Duration::from_millis(10));
        if res < 0 {
            record_failure(failure, format!("main loop iterate failed: errno {}", -res));
            break;
        }
        if lock_or_recover(failure).is_some() {
            break;
        }
        // Callbacks only run inside iterate(), so borrowing between
        // iterations cannot conflict.
        //
        // This check has to come before the cap: a failing writer stops
        // advancing samples_written, so the cap below can never fire on one.
        // Without this, a disk that fills while the GUI is gone leaves the
        // thread looping forever with two capture streams open.
        if let Some(msg) = take_write_error(&raw_slot, &filtered_slot) {
            record_failure(failure, msg);
            break;
        }
        // Overshoot is at most one callback of samples before the next
        // check lands here.
        if raw_slot.borrow().samples_written >= MAX_RECORDING_SAMPLES
            || filtered_slot.borrow().samples_written >= MAX_RECORDING_SAMPLES
        {
            record_failure(
                failure,
                format!(
                    "recording reached the {} s safety cap",
                    MAX_RECORDING_SAMPLES / SAMPLE_RATE as u64
                ),
            );
            break;
        }
        if !ready_sent && ready.raw.get() && ready.filtered.get() {
            ready_sent = true;
            let _ = ready_tx.send(Ok(()));
        }
    }

    // The loop above tests `running` before it drains, so a stop landing in
    // the same 10 ms window as the failing write would leave the cause in the
    // slot: finalized over, and reported as a clean recording.
    if let Some(msg) = take_write_error(&raw_slot, &filtered_slot) {
        record_failure(failure, msg);
    }

    if !ready_sent {
        let msg = lock_or_recover(failure)
            .clone()
            .unwrap_or_else(|| "recorder stopped before becoming ready".to_string());
        let _ = ready_tx.send(Err(msg));
    }

    // Finalize both writers: take them out of the slots so a write error
    // (e.g. disk full) is observable. hound's Drop would finalize silently.
    for slot in [&raw_slot, &filtered_slot] {
        if let Some(writer) = slot.borrow_mut().writer.take()
            && let Err(e) = writer.finalize()
        {
            record_failure(failure, format!("WAV finalize failed: {e}"));
        }
    }

    // Both streams and both listeners stay bound to the end of this
    // function, so they unwind in reverse declaration order: each listener
    // drops immediately before the stream it was added to. That order is
    // load-bearing — `pw_stream_destroy` frees the memory holding the hook
    // list, and a listener dropped afterwards unlinks its hook through it.
    // Do not "hold" them with `let _ = (..)`: `_` binds nothing, so that
    // drops the streams on the spot and leaves the listeners to outlive
    // them, which is the exact inversion this comment exists to prevent.
    mainloop.quit();
    Ok(())
}

/// Decode F32LE bytes from the capture buffer and append each sample to the
/// stream's `WavWriter`. Checkpoint-flushes the header every
/// [`FLUSH_INTERVAL_SAMPLES`] so a hard kill loses at most ~1 s of trailing
/// audio instead of the whole file (hound's reader trusts the data-size
/// header, which stays 0 until a flush).
fn process_capture(stream: &pipewire::stream::Stream, data: &mut Rc<RefCell<CaptureSlot>>) {
    let Some(mut buffer) = stream.dequeue_buffer() else {
        return;
    };
    let datas = buffer.datas_mut();
    if datas.is_empty() {
        return;
    }
    let chunk = datas[0].chunk();
    let offset = chunk.offset() as usize;
    let size = chunk.size() as usize;
    let Some(slice) = datas[0].data() else { return };
    if size == 0 || offset + size > slice.len() {
        return;
    }

    let bytes = &slice[offset..offset + size];
    let mut slot = data.borrow_mut();
    let mut since_flush = slot.samples_since_flush;
    let mut written = slot.samples_written;
    let Some(writer) = slot.writer.as_mut() else {
        return;
    };

    let mut write_error = None;
    for frame in bytes.chunks_exact(4) {
        let sample = f32::from_le_bytes([frame[0], frame[1], frame[2], frame[3]]);
        if let Err(e) = writer.write_sample(sample) {
            // Disk full or I/O error. Leave the writer in place so finalize
            // still runs on what was captured, and hand the cause to the
            // loop — it cannot be inferred from the counters, which stop
            // advancing precisely because the writes stopped landing.
            write_error = Some(format!("WAV write failed: {e}"));
            break;
        }
        since_flush += 1;
        written += 1;
        if since_flush >= FLUSH_INTERVAL_SAMPLES {
            if let Err(e) = writer.flush() {
                // Not ignorable: hound rewinds to the header, updates it,
                // flushes, and only then seeks back, so a failure here
                // leaves the cursor inside the header. Further writes still
                // report Ok (they only fill the BufWriter) while landing on
                // top of the audio already captured. Stop at the first
                // failure — the writer's position is no longer trustworthy.
                write_error = Some(format!("WAV checkpoint flush failed: {e}"));
                break;
            }
            since_flush = 0;
        }
    }
    slot.samples_since_flush = since_flush;
    slot.samples_written = written;
    if write_error.is_some() {
        slot.write_error = write_error;
    }
}

fn open_wav_writer(
    path: &Path,
) -> Result<hound::WavWriter<BufWriter<File>>, Box<dyn std::error::Error + Send + Sync>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // These are unprocessed recordings of the user's microphone, so they
    // are owner-only. `mode` covers files this call creates; the explicit
    // set_permissions also narrows one an earlier version left at 0644,
    // since the paths are fixed and get reused.
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    Ok(hound::WavWriter::new(BufWriter::new(file), WAV_SPEC)?)
}

// ---------------------------------------------------------------------------
// NativePlayer
// ---------------------------------------------------------------------------

/// Handle to the player thread owning one playback stream.
pub struct NativePlayer {
    running: Arc<AtomicBool>,
    failure: Arc<Mutex<Option<String>>>,
    eof: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl NativePlayer {
    /// Load `path` and start playing it back on the default sink. Up-front
    /// WAV spec validation: a foreign 44.1 kHz / stereo / PCM file would
    /// otherwise pitch-shift or clip silently.
    pub fn start(path: &Path) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let samples = load_wav(path)?;

        let running = Arc::new(AtomicBool::new(true));
        let running_clone = running.clone();
        let failure = Arc::new(Mutex::new(None));
        let failure_clone = failure.clone();
        let eof = Arc::new(AtomicBool::new(false));
        let eof_clone = eof.clone();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();

        let thread = thread::spawn(move || {
            if let Err(e) = run_player(
                samples,
                running_clone,
                &ready_tx,
                &failure_clone,
                &eof_clone,
            ) {
                record_failure(&failure_clone, format!("player error: {e}"));
                let _ = ready_tx.send(Err(e.to_string()));
                eprintln!("[NativePlayer] error: {e}");
            }
        });

        match ready_rx.recv_timeout(PIPELINE_READY_TIMEOUT) {
            Ok(Ok(())) => Ok(Self {
                running,
                failure,
                eof,
                thread: Some(thread),
            }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(format!("Player setup failed: {e}").into())
            }
            Err(_) => {
                running.store(false, Ordering::SeqCst);
                Err(format!(
                    "Player did not become ready within {:?}",
                    PIPELINE_READY_TIMEOUT
                )
                .into())
            }
        }
    }

    /// Failure-aware liveness: returns true once the thread has exited for
    /// *any* reason — EOF reached, a stream/core error, or an unexpected
    /// thread exit while `running` is still set. EOF-only would be a
    /// liveness regression versus the previous `try_wait` on a child.
    pub fn is_finished(&self) -> bool {
        if self.eof.load(Ordering::SeqCst) {
            return true;
        }
        if self.failure().is_some() {
            return true;
        }
        let thread_exited = self.thread.as_ref().is_some_and(JoinHandle::is_finished);
        thread_exited && self.running.load(Ordering::SeqCst)
    }

    /// Why the player died, if it has. A clean end-of-clip exit is not a
    /// death: reaching EOF finishes the thread with `running` still set, so
    /// without this short-circuit that normal completion would be reported as
    /// an unexpected exit. Only an exit that is neither EOF nor a recorded
    /// error (e.g. a thread panic) is genuinely unexpected.
    pub fn failure(&self) -> Option<String> {
        if let Some(msg) = lock_or_recover(&self.failure).clone() {
            return Some(msg);
        }
        if self.eof.load(Ordering::SeqCst) {
            return None;
        }
        let thread_exited = self.thread.as_ref().is_some_and(JoinHandle::is_finished);
        (self.running.load(Ordering::SeqCst) && thread_exited)
            .then(|| "player thread exited unexpectedly".to_string())
    }

    /// Signal the player thread to quit and join it. A second call is a
    /// no-op.
    pub fn stop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        if let Some(handle) = self.thread.take()
            && handle.join().is_err()
        {
            eprintln!("Player thread panicked during shutdown");
        }
    }
}

impl Drop for NativePlayer {
    fn drop(&mut self) {
        if self.thread.is_some() {
            self.stop();
        }
    }
}

struct PlayerData {
    samples: Vec<f32>,
    cursor: usize,
}

fn run_player(
    samples: Vec<f32>,
    running: Arc<AtomicBool>,
    ready_tx: &mpsc::Sender<Result<(), String>>,
    failure: &Arc<Mutex<Option<String>>>,
    eof: &Arc<AtomicBool>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    pipewire::init();

    let mainloop = MainLoopBox::new(None)?;
    let context = ContextBox::new(mainloop.loop_(), None)?;
    let core = context.connect(None)?;

    let format_pod = build_audio_format_pod(AudioFormat::F32LE, SAMPLE_RATE, 1, Some(&[MONO]))?;
    let pod = Pod::from_bytes(&format_pod).ok_or("Failed to build player format pod")?;

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

    let props = properties! {
        *pipewire::keys::MEDIA_TYPE => "Audio",
        *pipewire::keys::MEDIA_CATEGORY => "Playback",
        *pipewire::keys::MEDIA_ROLE => "Communication",
        *pipewire::keys::NODE_NAME => "spectral-playback",
        *pipewire::keys::NODE_DESCRIPTION => "Spectral Voice-Test Playback",
    };

    let stream = StreamBox::new(&core, "spectral-playback", props)?;

    let data = Rc::new(RefCell::new(PlayerData { samples, cursor: 0 }));
    let ready = Rc::new(Cell::new(false));
    let failure_for_cb = failure.clone();
    let ready_for_cb = ready.clone();
    let _listener = stream
        .add_local_listener_with_user_data(data)
        .state_changed(move |_stream, _data, _old, new| match new {
            StreamState::Error(msg) => {
                record_failure(&failure_for_cb, format!("playback stream error: {msg}"));
            }
            StreamState::Paused | StreamState::Streaming => ready_for_cb.set(true),
            _ => {}
        })
        .param_changed({
            let failure = failure.clone();
            move |_stream, _data, id, param| {
                verify_granted_format(id, param, &failure, "playback stream");
            }
        })
        .process({
            let eof = eof.clone();
            move |stream, data| process_playback(stream, data, &eof)
        })
        .register()?;

    // AUTOCONNECT so it lands on the default sink, MAP_BUFFERS, RT_PROCESS:
    // the callback only reads from a preloaded Vec — no I/O, RT-safe.
    stream.connect(
        Direction::Output,
        None,
        StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS | StreamFlags::RT_PROCESS,
        &mut [pod],
    )?;

    let mut ready_sent = false;
    while running.load(Ordering::SeqCst) {
        let res = mainloop.loop_().iterate(Duration::from_millis(10));
        if res < 0 {
            record_failure(failure, format!("main loop iterate failed: errno {}", -res));
            break;
        }
        if lock_or_recover(failure).is_some() {
            break;
        }
        if eof.load(Ordering::SeqCst) {
            break;
        }
        if !ready_sent && ready.get() {
            ready_sent = true;
            let _ = ready_tx.send(Ok(()));
        }
    }

    if !ready_sent {
        let msg = lock_or_recover(failure)
            .clone()
            .unwrap_or_else(|| "player stopped before becoming ready".to_string());
        let _ = ready_tx.send(Err(msg));
    }

    mainloop.quit();
    Ok(())
}

/// Write f32 frames from the preloaded sample Vec into the output buffer.
/// On exhaustion, fill silence and set `eof` so the main loop can quit and
/// [`NativePlayer::is_finished`] reports true. Mirrors `process_source_buffer`
/// in `pipewire.rs`.
fn process_playback(
    stream: &pipewire::stream::Stream,
    data: &mut Rc<RefCell<PlayerData>>,
    eof: &Arc<AtomicBool>,
) {
    let Some(mut buffer) = stream.dequeue_buffer() else {
        return;
    };
    let requested = buffer.requested() as usize;
    let datas = buffer.datas_mut();
    if datas.is_empty() {
        return;
    }
    let Some(slice) = datas[0].data() else { return };

    let max_samples = slice.len() / 4;
    let mut state = data.borrow_mut();
    let available = state.samples.len().saturating_sub(state.cursor);

    let wrote = if available > 0 {
        let n = available.min(max_samples);
        for i in 0..n {
            let s = state.samples[state.cursor + i];
            slice[i * 4..i * 4 + 4].copy_from_slice(&s.to_le_bytes());
        }
        state.cursor += n;
        n
    } else {
        eof.store(true, Ordering::SeqCst);
        let n = if requested > 0 {
            requested.min(max_samples)
        } else {
            max_samples
        };
        slice[..n * 4].fill(0);
        n
    };

    let chunk = datas[0].chunk_mut();
    *chunk.offset_mut() = 0;
    *chunk.size_mut() = (wrote * 4) as u32;
    *chunk.stride_mut() = 4;
}

/// Open `path` and validate its spec is mono 48 kHz 32-bit float, then
/// collect `samples::<f32>()` stopping at the first `Err` — a recording
/// killed mid-write has a truncated payload and hound yields `Err` past the
/// valid tail.
fn load_wav(path: &Path) -> Result<Vec<f32>, Box<dyn std::error::Error + Send + Sync>> {
    let reader = hound::WavReader::open(path)?;
    let spec = reader.spec();
    if spec != WAV_SPEC {
        return Err(format!(
            "Unsupported WAV spec: {:?} (expected mono 48 kHz 32-bit float)",
            spec
        )
        .into());
    }
    let mut out: Vec<f32> = Vec::new();
    for sample in reader.into_samples::<f32>() {
        match sample {
            Ok(s) => out.push(s),
            Err(_) => break,
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Tests (no PipeWire needed)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_wav_path(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(name);
        p
    }

    #[test]
    fn wav_spec_roundtrips() {
        let path = tmp_wav_path("spectral_recorder_roundtrip.wav");
        let samples: Vec<f32> = (0..1000).map(|i| (i as f32) / 1000.0).collect();

        {
            let mut writer =
                hound::WavWriter::new(BufWriter::new(File::create(&path).unwrap()), WAV_SPEC)
                    .unwrap();
            for &s in &samples {
                writer.write_sample(s).unwrap();
            }
            writer.finalize().unwrap();
        }

        let reader = hound::WavReader::open(&path).unwrap();
        assert_eq!(reader.spec(), WAV_SPEC);
        let loaded: Vec<f32> = reader.into_samples::<f32>().map(|s| s.unwrap()).collect();
        assert_eq!(loaded.len(), samples.len());
        for (a, b) in samples.iter().zip(loaded.iter()) {
            assert!((a - b).abs() < 1e-6, "sample mismatch: {a} vs {b}");
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn player_loads_truncated_wav_without_panicking() {
        let path = tmp_wav_path("spectral_recorder_truncated.wav");
        let samples: Vec<f32> = (0..2000).map(|i| (i as f32) / 2000.0).collect();

        // Write a valid WAV, finalize, then chop the tail off the file so
        // the data chunk claims more than is actually present.
        {
            let mut writer =
                hound::WavWriter::new(BufWriter::new(File::create(&path).unwrap()), WAV_SPEC)
                    .unwrap();
            for &s in &samples {
                writer.write_sample(s).unwrap();
            }
            writer.finalize().unwrap();
        }
        let full_len = std::fs::metadata(&path).unwrap().len();
        let chopped = full_len - 1024;
        let f = OpenOptions::new().write(true).open(&path).unwrap();
        f.set_len(chopped).unwrap();
        drop(f);

        // load_wav must stop at the first Err and return the partial Vec.
        let loaded = load_wav(&path).expect("truncated WAV should still load partially");
        assert!(loaded.len() < samples.len(), "expected a partial load");
        assert!(
            !loaded.is_empty(),
            "expected some samples before the truncation"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn player_rejects_wrong_spec() {
        let path = tmp_wav_path("spectral_recorder_44k.wav");
        let bad_spec = hound::WavSpec {
            channels: 1,
            sample_rate: 44100,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        {
            let mut writer =
                hound::WavWriter::new(BufWriter::new(File::create(&path).unwrap()), bad_spec)
                    .unwrap();
            writer.write_sample(0.0_f32).unwrap();
            writer.finalize().unwrap();
        }
        let err = load_wav(&path).expect_err("44.1 kHz file must be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("Unsupported WAV spec") && msg.contains("44100"),
            "expected a clear spec-rejection error, got: {msg}"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// Sanity check that the shared constant matches the same value hound would
    /// build for the recorder — keeps the two halves from drifting.
    #[test]
    fn wav_spec_matches_pw_record_defaults() {
        assert_eq!(WAV_SPEC.channels, 1);
        assert_eq!(WAV_SPEC.sample_rate, SAMPLE_RATE);
        assert_eq!(WAV_SPEC.bits_per_sample, 32);
        assert_eq!(WAV_SPEC.sample_format, hound::SampleFormat::Float);
    }

    /// The daemon-side cap is a backstop for an orphaned recording, not a
    /// limit a normal voice test can hit: the GUI stops recordings at
    /// 15 s, and the cap must keep a comfortable margin above that.
    #[test]
    fn recording_cap_leaves_headroom_over_the_gui_stop() {
        assert_eq!(MAX_RECORDING_SAMPLES % u64::from(SAMPLE_RATE), 0);
        let cap_seconds = MAX_RECORDING_SAMPLES / u64::from(SAMPLE_RATE);
        assert!(
            cap_seconds >= 4 * 15,
            "cap of {cap_seconds} s leaves too little margin over the GUI's 15 s stop"
        );
    }

    /// Build a `NativePlayer` whose thread has already exited, so the
    /// liveness predicates can be exercised without a PipeWire mainloop.
    /// `running` stays `true` to mirror a real thread that returned on its
    /// own (EOF or panic) rather than being told to stop.
    fn exited_player(eof: bool, failure: Option<String>) -> NativePlayer {
        let handle = thread::spawn(|| {});
        while !handle.is_finished() {
            thread::yield_now();
        }
        NativePlayer {
            running: Arc::new(AtomicBool::new(true)),
            failure: Arc::new(Mutex::new(failure)),
            eof: Arc::new(AtomicBool::new(eof)),
            thread: Some(handle),
        }
    }

    /// Reaching the end of the clip finishes the player thread with `running`
    /// still set; that clean exit must not masquerade as a failure. Guards
    /// the "Playback stopped: player thread exited unexpectedly" regression.
    #[test]
    fn eof_exit_is_a_clean_end_not_a_failure() {
        let player = exited_player(true, None);
        assert!(player.is_finished(), "EOF must count as finished");
        assert_eq!(
            player.failure(),
            None,
            "a normal end-of-clip exit is not a failure"
        );
    }

    /// A thread that vanished without reaching EOF and without recording an
    /// error (e.g. a panic) is genuinely unexpected and must still surface.
    #[test]
    fn exit_without_eof_or_recorded_error_is_reported() {
        let player = exited_player(false, None);
        assert_eq!(
            player.failure().as_deref(),
            Some("player thread exited unexpectedly"),
            "an exit that is neither EOF nor a recorded error is unexpected"
        );
    }

    /// A recorded stream/core error always wins, even once EOF has been set,
    /// so a real fault is never hidden behind the clean-exit short-circuit.
    #[test]
    fn recorded_error_takes_precedence_over_eof() {
        let player = exited_player(true, Some("playback stream error: boom".to_string()));
        assert_eq!(
            player.failure().as_deref(),
            Some("playback stream error: boom")
        );
    }
}
