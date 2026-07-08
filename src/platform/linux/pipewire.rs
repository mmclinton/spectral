//! Native PipeWire Audio Pipeline
//!
//! This module provides a native PipeWire client for audio capture and playback,
//! using direct stream connections for low-latency noise reduction.

use super::audio_processor::AudioProcessor;
use crate::state::lock_or_recover;
use pipewire::spa::{
    param::audio::{AudioFormat, AudioInfoRaw},
    pod::Pod,
    utils::Direction,
};
use pipewire::{
    context::ContextBox,
    core::PW_ID_CORE,
    main_loop::MainLoopBox,
    properties::properties,
    stream::{StreamBox, StreamFlags, StreamState},
};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// Audio sample rate in Hz (48kHz is standard for PipeWire)
pub const SAMPLE_RATE: u32 = 48000;

pub const OUTPUT_CHANNELS: u32 = 1;

/// RNNoise requires exactly 480 samples (10ms at 48kHz) per frame.
pub const RNNOISE_FRAME_SIZE: usize = 480;

/// Channels beyond this will be clamped.
/// TODO: Validate behavior with devices that have more than 8 channels
pub const MAX_INPUT_CHANNELS: u32 = 8;

/// Scaling factor for converting f32 PCM [-1.0, 1.0] to RNNoise's expected i16 range.
/// RNNoise expects samples in the range [-32768.0, 32767.0] as f32 values.
pub const RNNOISE_SCALE_FACTOR: f32 = 32768.0;

/// Maximum time to wait for the pipeline thread to finish its PipeWire setup.
const PIPELINE_READY_TIMEOUT: Duration = Duration::from_secs(5);

struct PipelineData {
    processor: AudioProcessor,
    input_buffer: Vec<f32>,
    output_buffer: Vec<f32>,
    input_channels: u32,
    mono_scratch: Vec<f32>,
    processed_scratch: Vec<f32>,
}

impl PipelineData {
    fn new(processor: AudioProcessor, input_channels: u32) -> Self {
        // Clamp channels to valid range
        // TODO: Validate that channel clamping produces acceptable audio quality
        let channels = input_channels.clamp(1, MAX_INPUT_CHANNELS);
        Self {
            processor,
            input_buffer: Vec::with_capacity(RNNOISE_FRAME_SIZE * channels as usize * 4),
            output_buffer: Vec::with_capacity(RNNOISE_FRAME_SIZE * 4),
            input_channels: channels,
            mono_scratch: vec![0.0; RNNOISE_FRAME_SIZE],
            processed_scratch: vec![0.0; RNNOISE_FRAME_SIZE],
        }
    }
}

pub struct NativeAudioPipeline {
    running: Arc<AtomicBool>,
    failure: Arc<Mutex<Option<String>>>,
    pipeline_thread: Option<JoinHandle<()>>,
}

/// First error wins: later failures are usually cascades of the first.
fn record_failure(slot: &Mutex<Option<String>>, msg: String) {
    let mut guard = lock_or_recover(slot);
    if guard.is_none() {
        *guard = Some(msg);
    }
}

impl NativeAudioPipeline {
    /// Start the native audio pipeline.
    ///
    /// # Arguments
    /// * `processor` - Audio processor for noise reduction
    /// * `source_node` - PipeWire node name of the source device
    /// * `sink_node` - PipeWire node name of the sink
    /// * `input_channels` - Number of channels on the input device
    ///
    /// TODO: Validate that PipeWire correctly negotiates the channel count
    /// for various device types (USB mics, built-in mics, audio interfaces)
    pub fn start(
        processor: AudioProcessor,
        source_node: &str,
        sink_node: &str,
        input_channels: u32,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let running = Arc::new(AtomicBool::new(true));
        let running_clone = running.clone();
        let failure = Arc::new(Mutex::new(None));
        let failure_clone = failure.clone();
        let source_node = source_node.to_string();
        let sink_node = sink_node.to_string();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();

        let pipeline_thread = thread::spawn(move || {
            if let Err(e) = run_pipeline(
                processor,
                &source_node,
                &sink_node,
                running_clone,
                input_channels,
                &ready_tx,
                &failure_clone,
            ) {
                record_failure(&failure_clone, format!("pipeline error: {e}"));
                let _ = ready_tx.send(Err(e.to_string()));
                eprintln!("[Native Pipeline] ✗ Pipeline error: {}", e);
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
                Err(format!("Pipeline setup failed: {}", e).into())
            }
            Err(_) => {
                // Don't join: a wedged setup thread would hang the daemon.
                running.store(false, Ordering::SeqCst);
                Err(format!(
                    "Pipeline did not become ready within {:?}",
                    PIPELINE_READY_TIMEOUT
                )
                .into())
            }
        }
    }

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

fn build_audio_format_pod(
    format: AudioFormat,
    rate: u32,
    channels: u32,
) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    let mut audio_info = AudioInfoRaw::new();
    audio_info.set_format(format);
    audio_info.set_rate(rate);
    audio_info.set_channels(channels);

    let values: Vec<u8> = pipewire::spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &pipewire::spa::pod::Value::Object(pipewire::spa::pod::Object {
            type_: pipewire::spa::sys::SPA_TYPE_OBJECT_Format,
            id: pipewire::spa::param::ParamType::EnumFormat.as_raw(),
            properties: audio_info.into(),
        }),
    )
    .map_err(|e| format!("Failed to serialize audio format: {:?}", e))?
    .0
    .into_inner();

    Ok(values)
}

fn run_pipeline(
    processor: AudioProcessor,
    source_node: &str,
    sink_node: &str,
    running: Arc<AtomicBool>,
    input_channels: u32,
    ready_tx: &mpsc::Sender<Result<(), String>>,
    failure: &Arc<Mutex<Option<String>>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    pipewire::init();

    let mainloop = MainLoopBox::new(None)?;
    let context = ContextBox::new(mainloop.loop_(), None)?;
    let core = context.connect(None)?;

    let channels = input_channels.clamp(1, MAX_INPUT_CHANNELS);

    // Must use Rc because PipeWire callbacks are !Send
    let pipeline_data = Rc::new(RefCell::new(PipelineData::new(processor, channels)));

    // TODO: Verify PipeWire format negotiation works correctly for mono (1 channel) devices
    let capture_format = build_audio_format_pod(AudioFormat::F32LE, SAMPLE_RATE, channels)?;
    let playback_format = build_audio_format_pod(AudioFormat::F32LE, SAMPLE_RATE, OUTPUT_CHANNELS)?;

    // "target.object" used since NODE_TARGET isn't exposed in the Rust bindings
    let capture_props = properties! {
        *pipewire::keys::MEDIA_TYPE => "Audio",
        *pipewire::keys::MEDIA_CATEGORY => "Capture",
        *pipewire::keys::MEDIA_ROLE => "Communication",
        *pipewire::keys::NODE_NAME => "spectral-native-capture",
        *pipewire::keys::NODE_DESCRIPTION => "Spectral Native Capture",
        "target.object" => source_node,
    };

    let capture_stream = StreamBox::new(&core, "spectral-capture", capture_props)?;

    let capture_data = pipeline_data.clone();
    let capture_failure = failure.clone();
    let _capture_listener = capture_stream
        .add_local_listener_with_user_data(capture_data)
        .state_changed(move |_stream, _data, _old, new| {
            if let StreamState::Error(msg) = new {
                record_failure(&capture_failure, format!("capture stream error: {msg}"));
            }
        })
        .process(|stream, data| {
            process_capture_buffer(stream, data);
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

    let playback_props = properties! {
        *pipewire::keys::MEDIA_TYPE => "Audio",
        *pipewire::keys::MEDIA_CATEGORY => "Playback",
        *pipewire::keys::MEDIA_ROLE => "Communication",
        *pipewire::keys::NODE_NAME => "spectral-native-playback",
        *pipewire::keys::NODE_DESCRIPTION => "Spectral Native Playback",
        "target.object" => sink_node,
    };

    let playback_stream = StreamBox::new(&core, "spectral-playback", playback_props)?;

    let playback_data = pipeline_data.clone();
    let playback_failure = failure.clone();
    let _playback_listener = playback_stream
        .add_local_listener_with_user_data(playback_data)
        .state_changed(move |_stream, _data, _old, new| {
            if let StreamState::Error(msg) = new {
                record_failure(&playback_failure, format!("playback stream error: {msg}"));
            }
        })
        .process(|stream, data| {
            process_playback_buffer(stream, data);
        })
        .register()?;

    let playback_pod =
        Pod::from_bytes(&playback_format).ok_or("Failed to create playback format pod")?;
    playback_stream.connect(
        Direction::Output,
        None,
        StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS | StreamFlags::RT_PROCESS,
        &mut [playback_pod],
    )?;

    let _timer = mainloop.loop_().add_timer(|_| {});
    let _ = _timer.update_timer(
        Some(std::time::Duration::from_millis(100)),
        Some(std::time::Duration::from_millis(100)),
    );

    let core_failure = failure.clone();
    let _core_listener = core
        .add_listener_local()
        .error(move |id, _seq, res, message| {
            if id == PW_ID_CORE {
                record_failure(
                    &core_failure,
                    format!("PipeWire core error {res}: {message}"),
                );
            }
        })
        .register();

    let _ = ready_tx.send(Ok(()));

    while running.load(Ordering::SeqCst) {
        let res = mainloop
            .loop_()
            .iterate(std::time::Duration::from_millis(10));
        if res < 0 {
            record_failure(failure, format!("main loop iterate failed: errno {}", -res));
            break;
        }
        if lock_or_recover(failure).is_some() {
            break;
        }
    }

    mainloop.quit();

    Ok(())
}

/// Mix N channels down to mono.
///
/// This function handles any number of input channels (1-8) and produces
/// a mono output scaled to the i16 range expected by RNNoise.
///
/// TODO: Validate audio quality for various channel configurations:
/// - Mono (1 channel): Should pass through directly
/// - Stereo (2 channels): Standard L+R/2 mixing
/// - Surround (5.1/7.1): May need weighted mixing for better voice capture
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
    let channel_divisor = channels as f32;

    for i in 0..frame_count {
        if i >= output.len() {
            break;
        }

        // TODO: Consider weighted mixing for surround sound (center channel boost for voice)
        let sum: f32 = (0..channels)
            .map(|ch| {
                let idx = i * channels + ch;
                if idx < input.len() { input[idx] } else { 0.0 }
            })
            .sum();

        // Scale to i16 range for RNNoise
        output[i] = (sum / channel_divisor) * RNNOISE_SCALE_FACTOR;
    }
}

fn process_capture_buffer(stream: &pipewire::stream::Stream, data: &mut Rc<RefCell<PipelineData>>) {
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

            let valid_data = &slice[chunk_offset..chunk_offset + chunk_size];
            let mut pipeline_data = data.borrow_mut();
            let PipelineData {
                processor,
                input_buffer,
                output_buffer,
                input_channels,
                mono_scratch,
                processed_scratch,
            } = &mut *pipeline_data;
            let channels = *input_channels as usize;

            for audio_chunk in valid_data.chunks_exact(4) {
                let sample = f32::from_le_bytes([
                    audio_chunk[0],
                    audio_chunk[1],
                    audio_chunk[2],
                    audio_chunk[3],
                ]);
                input_buffer.push(sample);
            }

            // Process complete frames (480 samples per channel) in the
            // preallocated scratch buffers — the RT thread must not allocate.
            let frame_size = RNNOISE_FRAME_SIZE * channels;
            while input_buffer.len() >= frame_size {
                // Convert N channels to mono and scale to i16 range for RNNoise
                mix_to_mono(&input_buffer[..frame_size], channels, mono_scratch);
                processor.process_frame(mono_scratch, processed_scratch);

                // Scale back to f32 PCM range [-1.0, 1.0]
                for sample in processed_scratch.iter_mut() {
                    *sample /= RNNOISE_SCALE_FACTOR;
                }

                output_buffer.extend_from_slice(processed_scratch);
                input_buffer.drain(..frame_size);
            }
        }
    }
}

fn process_playback_buffer(
    stream: &pipewire::stream::Stream,
    data: &mut Rc<RefCell<PipelineData>>,
) {
    if let Some(mut buffer) = stream.dequeue_buffer() {
        let datas = buffer.datas_mut();
        if datas.is_empty() {
            return;
        }

        if let Some(slice) = datas[0].data() {
            let mut pipeline_data = data.borrow_mut();

            let max_samples = slice.len() / 4;
            let available = pipeline_data.output_buffer.len().min(max_samples);

            for (i, sample) in pipeline_data.output_buffer[..available].iter().enumerate() {
                slice[i * 4..i * 4 + 4].copy_from_slice(&sample.to_le_bytes());
            }
            pipeline_data.output_buffer.drain(..available);
            let bytes_written = available * 4;

            // CRITICAL: Set the chunk size to indicate how many bytes we wrote
            let chunk = datas[0].chunk_mut();
            *chunk.offset_mut() = 0;
            *chunk.size_mut() = bytes_written as u32;
            *chunk.stride_mut() = 4; // 4 bytes per sample (f32)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audio_format_pod_creation() {
        let result = build_audio_format_pod(AudioFormat::F32LE, 48000, 2);
        assert!(result.is_ok(), "Should be able to create audio format pod");
        let pod_bytes = result.unwrap();
        assert!(!pod_bytes.is_empty(), "Pod should have content");
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
}
