//! Native PipeWire WAV recorder for the voice-test A/B path.
//!
//! A self-contained type that mirrors `NativeAudioPipeline`'s proven
//! lifecycle: a dedicated mainloop thread, an `mpsc` ready-handshake with
//! [`super::pipewire::PIPELINE_READY_TIMEOUT`], a `running:
//! Arc<AtomicBool>` flag, a `failure: Arc<Mutex<Option<String>>>` slot
//! (first error wins), and a `Drop` that calls `stop()`. Replaces the
//! former `pw-record` shell-out so the daemon no longer depends on the
//! PipeWire *tools* package at runtime — only the libraries it links.
//!
//! WAV encoding uses `hound` (pure Rust, already transitively in
//! `Cargo.lock` via `nnnoiseless`); see <https://docs.rs/hound>.
//!
//! Both files are mono 48 kHz 32-bit float (matching `pw-record --channels 1
//! --format f32`). Recording captures the raw f32 the graph delivers — no
//! i16 scaling, so the file is bit-identical to what `pw-record` produced.

use super::pipewire::{PIPELINE_READY_TIMEOUT, SAMPLE_RATE, build_audio_format_pod};
use crate::state::lock_or_recover;
use pipewire::spa::{param::audio::AudioFormat, pod::Pod, utils::Direction};
use pipewire::{
    context::ContextBox,
    main_loop::MainLoopBox,
    properties::properties,
    stream::{StreamBox, StreamFlags, StreamState},
};
use std::cell::{Cell, RefCell};
use std::fs::{File, OpenOptions};
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// PipeWire's mono channel position.
const MONO: u32 = pipewire::spa::sys::SPA_AUDIO_CHANNEL_MONO;

/// WAV spec used by the recorder: mono, 48 kHz, 32-bit float.
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

/// First error wins: later failures are usually cascades of the first.
fn record_failure(slot: &Mutex<Option<String>>, msg: String) {
    let mut guard = lock_or_recover(slot);
    if guard.is_none() {
        *guard = Some(msg);
    }
}

// ---------------------------------------------------------------------------
// NativeRecorder
// ---------------------------------------------------------------------------

/// Per-stream state carried inside the recorder's PipeWire callbacks.
struct CaptureSlot {
    writer: Option<hound::WavWriter<BufWriter<File>>>,
    samples_since_flush: u64,
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
                running.store(false, Ordering::SeqCst);
                Err(format!(
                    "Recorder did not become ready within {:?}",
                    PIPELINE_READY_TIMEOUT
                )
                .into())
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

    // Surface any core/proxy error: a missing target.object with
    // dont-reconnect=true reports here, which is exactly the case we want
    // to fail loudly instead of silently recording the default source.
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
    }));
    let raw_props = properties! {
        *pipewire::keys::MEDIA_TYPE => "Audio",
        *pipewire::keys::MEDIA_CATEGORY => "Capture",
        *pipewire::keys::MEDIA_ROLE => "Communication",
        *pipewire::keys::NODE_NAME => "spectral-recorder-raw",
        *pipewire::keys::NODE_DESCRIPTION => "Spectral Voice-Test Recorder",
        "target.object" => source_node,
        // Without this, a vanished target.object makes WirePlumber
        // silently move the stream to the default source — a wrong-node
        // recording, not an error. With it, a missing target is a hard
        // error that surfaces through the failure slot.
        "node.dont-reconnect" => "true",
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
    }));
    let filtered_props = properties! {
        *pipewire::keys::MEDIA_TYPE => "Audio",
        *pipewire::keys::MEDIA_CATEGORY => "Capture",
        *pipewire::keys::MEDIA_ROLE => "Communication",
        *pipewire::keys::NODE_NAME => "spectral-recorder-filtered",
        *pipewire::keys::NODE_DESCRIPTION => "Spectral Voice-Test Recorder",
        "target.object" => mic_name,
        "node.dont-reconnect" => "true",
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
        if !ready_sent && ready.raw.get() && ready.filtered.get() {
            ready_sent = true;
            let _ = ready_tx.send(Ok(()));
        }
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

    // Hold the streams and listeners through the end of the function so
    // their PipeWire objects stay registered with the mainloop above.
    let _ = (raw_stream, filtered_stream);

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
    let Some(writer) = slot.writer.as_mut() else {
        return;
    };

    for frame in bytes.chunks_exact(4) {
        let sample = f32::from_le_bytes([frame[0], frame[1], frame[2], frame[3]]);
        if writer.write_sample(sample).is_err() {
            // Disk full / I/O error: stop writing but leave the slot alive
            // so the recorder loop can surface the failure via finalize.
            break;
        }
        since_flush += 1;
        if since_flush >= FLUSH_INTERVAL_SAMPLES {
            let _ = writer.flush();
            since_flush = 0;
        }
    }
    slot.samples_since_flush = since_flush;
}

fn open_wav_writer(
    path: &Path,
) -> Result<hound::WavWriter<BufWriter<File>>, Box<dyn std::error::Error + Send + Sync>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Truncate any stale file from a previous run; matches the previous
    // pw-record behavior of overwriting in place.
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)?;
    Ok(hound::WavWriter::new(BufWriter::new(file), WAV_SPEC)?)
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

    /// Sanity check that our constant matches the same value hound would
    /// build for the recorder — keeps the spec from drifting.
    #[test]
    fn wav_spec_matches_pw_record_defaults() {
        assert_eq!(WAV_SPEC.channels, 1);
        assert_eq!(WAV_SPEC.sample_rate, SAMPLE_RATE);
        assert_eq!(WAV_SPEC.bits_per_sample, 32);
        assert_eq!(WAV_SPEC.sample_format, hound::SampleFormat::Float);
    }
}
