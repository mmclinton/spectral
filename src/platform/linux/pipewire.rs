//! Native PipeWire Audio Pipeline
//!
//! This module provides a native PipeWire client for audio capture and playback,
//! using direct stream connections for low-latency noise reduction.

use super::audio_processor::AudioProcessor;
use pipewire::spa::{
    param::audio::{AudioFormat, AudioInfoRaw},
    pod::Pod,
    utils::Direction,
};
use pipewire::{
    context::ContextBox,
    main_loop::MainLoopBox,
    properties::properties,
    stream::{StreamBox, StreamFlags},
};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
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
    processor: Arc<AudioProcessor>,
    input_buffer: Vec<f32>,
    output_buffer: Vec<f32>,
    input_channels: u32,
}

impl PipelineData {
    fn new(processor: Arc<AudioProcessor>, input_channels: u32) -> Self {
        // Clamp channels to valid range
        // TODO: Validate that channel clamping produces acceptable audio quality
        let channels = input_channels.clamp(1, MAX_INPUT_CHANNELS);
        Self {
            processor,
            input_buffer: Vec::with_capacity(RNNOISE_FRAME_SIZE * channels as usize * 4),
            output_buffer: Vec::with_capacity(RNNOISE_FRAME_SIZE * 4),
            input_channels: channels,
        }
    }
}

pub struct NativeAudioPipeline {
    running: Arc<AtomicBool>,
    pipeline_thread: Option<JoinHandle<()>>,
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
        processor: Arc<AudioProcessor>,
        source_node: &str,
        sink_node: &str,
        input_channels: u32,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let running = Arc::new(AtomicBool::new(true));
        let running_clone = running.clone();
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
            ) {
                let _ = ready_tx.send(Err(e.to_string()));
                eprintln!("[Native Pipeline] ✗ Pipeline error: {}", e);
            }
        });

        match ready_rx.recv_timeout(PIPELINE_READY_TIMEOUT) {
            Ok(Ok(())) => Ok(Self {
                running,
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
    processor: Arc<AudioProcessor>,
    source_node: &str,
    sink_node: &str,
    running: Arc<AtomicBool>,
    input_channels: u32,
    ready_tx: &mpsc::Sender<Result<(), String>>,
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
    let _capture_listener = capture_stream
        .add_local_listener_with_user_data(capture_data)
        .state_changed(|_stream, _data, _old, _new| {})
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
    let _playback_listener = playback_stream
        .add_local_listener_with_user_data(playback_data)
        .state_changed(|_stream, _data, _old, _new| {})
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

    let _ = ready_tx.send(Ok(()));

    while running.load(Ordering::SeqCst) {
        let _ = mainloop
            .loop_()
            .iterate(std::time::Duration::from_millis(10));
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

            let input_channels = pipeline_data.input_channels as usize;

            for audio_chunk in valid_data.chunks_exact(4) {
                let sample = f32::from_le_bytes([
                    audio_chunk[0],
                    audio_chunk[1],
                    audio_chunk[2],
                    audio_chunk[3],
                ]);
                pipeline_data.input_buffer.push(sample);
            }

            // Process complete frames (480 samples per channel)
            let frame_size = RNNOISE_FRAME_SIZE * input_channels;
            while pipeline_data.input_buffer.len() >= frame_size {
                let frame: Vec<f32> = pipeline_data.input_buffer.drain(..frame_size).collect();

                // Convert N channels to mono and scale to i16 range for RNNoise
                let mut mono_samples = vec![0f32; RNNOISE_FRAME_SIZE];
                mix_to_mono(&frame, input_channels, &mut mono_samples);

                let mut processed_samples = vec![0f32; RNNOISE_FRAME_SIZE];
                pipeline_data
                    .processor
                    .process_audio_buffer(&mono_samples, &mut processed_samples);

                // Scale back to f32 PCM range [-1.0, 1.0]
                for sample in &mut processed_samples {
                    *sample /= RNNOISE_SCALE_FACTOR;
                }

                pipeline_data
                    .output_buffer
                    .extend_from_slice(&processed_samples);
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

            let bytes_written = if available > 0 {
                let output_samples: Vec<f32> =
                    pipeline_data.output_buffer.drain(..available).collect();

                for (i, &sample) in output_samples.iter().enumerate() {
                    let bytes = sample.to_le_bytes();
                    let offset = i * 4;
                    if offset + 4 <= slice.len() {
                        // Use safe slice copy instead of unsafe ptr operations
                        slice[offset..offset + 4].copy_from_slice(&bytes);
                    }
                }
                available * 4
            } else {
                0
            };

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
}
