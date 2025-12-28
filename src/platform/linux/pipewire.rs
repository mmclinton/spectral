//! Native PipeWire Audio Pipeline
//!
//! This module provides a native PipeWire client for audio capture and playback,
//! using direct stream connections for low-latency noise reduction.
//!
//! Benefits of native implementation:
//! - Lower latency: Direct buffer access without inter-process piping
//! - Better reliability: No dependency on external CLI tools being installed
//! - Precise control: Direct buffer sizing and format negotiation

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
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};

const SAMPLE_RATE: u32 = 48000;
const INPUT_CHANNELS: u32 = 2; // Stereo input from microphone
const OUTPUT_CHANNELS: u32 = 1; // Mono output after processing
const BUFFER_SIZE: usize = 480; // 10ms buffer at 48kHz - MUST match RNNoise FRAME_SIZE

struct PipelineData {
    processor: Arc<AudioProcessor>,
    input_buffer: Vec<f32>,
    output_buffer: Vec<f32>,
    prev_sample: f32,
    smoothing_alpha: f32,
}

impl PipelineData {
    fn new(processor: Arc<AudioProcessor>) -> Self {
        Self {
            processor,
            input_buffer: Vec::with_capacity(BUFFER_SIZE * INPUT_CHANNELS as usize * 4),
            output_buffer: Vec::with_capacity(BUFFER_SIZE * 4),
            prev_sample: 0.0,
            smoothing_alpha: 0.15,
        }
    }
}

pub struct NativeAudioPipeline {
    running: Arc<AtomicBool>,
    pipeline_thread: Option<JoinHandle<()>>,
}

impl NativeAudioPipeline {
    pub fn start(
        processor: Arc<AudioProcessor>,
        source_node: &str,
        sink_node: &str,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let running = Arc::new(AtomicBool::new(true));
        let running_clone = running.clone();
        let source_node = source_node.to_string();
        let sink_node = sink_node.to_string();

        let pipeline_thread = thread::spawn(move || {
            if let Err(e) = run_pipeline(processor, &source_node, &sink_node, running_clone) {
                eprintln!("[Native Pipeline] ✗ Pipeline error: {}", e);
            }
        });

        Ok(Self {
            running,
            pipeline_thread: Some(pipeline_thread),
        })
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
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    pipewire::init();

    let mainloop = MainLoopBox::new(None)?;
    let context = ContextBox::new(mainloop.loop_(), None)?;
    let core = context.connect(None)?;

    // Must use Rc because PipeWire callbacks are !Send
    let pipeline_data = Rc::new(RefCell::new(PipelineData::new(processor)));

    let capture_format = build_audio_format_pod(AudioFormat::F32LE, SAMPLE_RATE, INPUT_CHANNELS)?;
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

    while running.load(Ordering::SeqCst) {
        let _ = mainloop
            .loop_()
            .iterate(std::time::Duration::from_millis(10));
    }

    mainloop.quit();

    Ok(())
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

            for audio_chunk in valid_data.chunks_exact(4) {
                let sample = f32::from_le_bytes([
                    audio_chunk[0],
                    audio_chunk[1],
                    audio_chunk[2],
                    audio_chunk[3],
                ]);
                pipeline_data.input_buffer.push(sample);
            }

            // Process complete stereo frames (480 samples per channel = 960 samples)
            let frame_size = BUFFER_SIZE * INPUT_CHANNELS as usize;
            while pipeline_data.input_buffer.len() >= frame_size {
                let frame: Vec<f32> = pipeline_data.input_buffer.drain(..frame_size).collect();

                // Convert stereo to mono and scale to i16 range for RNNoise
                let mut mono_samples = vec![0f32; BUFFER_SIZE];
                for i in 0..BUFFER_SIZE {
                    let left = frame[i * 2];
                    let right = frame[i * 2 + 1];
                    let mono = (left + right) / 2.0;
                    mono_samples[i] = mono * 32768.0;
                }

                let mut processed_samples = vec![0f32; BUFFER_SIZE];
                pipeline_data
                    .processor
                    .process_audio_buffer(&mono_samples, &mut processed_samples);

                let smoothing_alpha = pipeline_data.smoothing_alpha;
                for sample in &mut processed_samples {
                    let smoothed = (1.0 - smoothing_alpha) * *sample
                        + smoothing_alpha * pipeline_data.prev_sample;
                    pipeline_data.prev_sample = smoothed;
                    *sample = smoothed / 32768.0;
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
                        // SAFETY: bounds checked above
                        unsafe {
                            std::ptr::copy_nonoverlapping(
                                bytes.as_ptr(),
                                slice.as_ptr().add(offset) as *mut u8,
                                4,
                            );
                        }
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
