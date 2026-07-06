use super::audio_devices::get_node_name;
use super::pipewire::{NativeAudioPipeline, RNNOISE_FRAME_SIZE};
use super::pulse::PulseContext;
use crate::state::{AppState, is_valid_mic_name, lock_or_recover, monitor_name, sink_name};
use nnnoiseless::DenoiseState;
use std::sync::{Arc, Mutex};

struct ProcessingContext {
    denoiser: Box<DenoiseState<'static>>,
    input_buffer: Vec<f32>,
    vad_state: f32,
    scratch_input: Vec<f32>,
    scratch_output: Vec<f32>,
}

pub struct AudioProcessor {
    state: Arc<AppState>,
    context: Mutex<ProcessingContext>,
}

impl AudioProcessor {
    pub fn new(state: Arc<AppState>) -> Self {
        Self {
            state,
            context: Mutex::new(ProcessingContext {
                denoiser: DenoiseState::new(),
                input_buffer: Vec::with_capacity(RNNOISE_FRAME_SIZE * 2),
                vad_state: 0.0,
                scratch_input: vec![0.0; RNNOISE_FRAME_SIZE],
                scratch_output: vec![0.0; RNNOISE_FRAME_SIZE],
            }),
        }
    }

    pub fn process_audio_buffer(&self, input: &[f32], output: &mut [f32]) {
        let threshold = *lock_or_recover(&self.state.threshold_val) as f32 / 100.0;

        let mut ctx_guard = lock_or_recover(&self.context);

        let ProcessingContext {
            denoiser,
            input_buffer,
            vad_state,
            scratch_input,
            scratch_output,
        } = &mut *ctx_guard;

        input_buffer.extend_from_slice(input);

        let mut out_idx = 0;

        while input_buffer.len() >= RNNOISE_FRAME_SIZE {
            for (i, sample) in input_buffer.drain(..RNNOISE_FRAME_SIZE).enumerate() {
                scratch_input[i] = sample;
            }

            // Apply RNNoise - processes audio in i16 range [-32768.0, 32767.0]
            // Returns VAD probability (0.0-1.0) indicating voice presence confidence
            let vad_probability = denoiser.process_frame(scratch_output, scratch_input);

            if threshold >= 0.5 {
                // Map threshold 50-100% to VAD cutoff 0.5-0.95
                let vad_threshold = (threshold - 0.5) * 0.9 + 0.5;

                let target_gate = if vad_probability > vad_threshold {
                    1.0
                } else {
                    0.0
                };

                // Smooth gate transitions: attack=0.50 (~20ms ramp), release=0.96 (~250ms decay)
                let attack_coef = 0.50;
                let release_coef = 0.96;

                if target_gate > 0.5 {
                    *vad_state = attack_coef * (*vad_state) + (1.0 - attack_coef);
                } else {
                    *vad_state *= release_coef;
                }

                let gain = *vad_state;
                for &sample in scratch_output.iter() {
                    if out_idx < output.len() {
                        output[out_idx] = sample * gain;
                        out_idx += 1;
                    }
                }
            } else {
                for &sample in scratch_output.iter() {
                    if out_idx < output.len() {
                        output[out_idx] = sample;
                        out_idx += 1;
                    }
                }
            }
        }

        while out_idx < output.len() {
            output[out_idx] = 0.0;
            out_idx += 1;
        }
    }
}

pub struct VirtualMic {
    _source_name: String,
    sink_module_id: Option<u32>,
    remap_module_id: Option<u32>,
    audio_pipeline: Option<NativeAudioPipeline>,
    _processor: Arc<AudioProcessor>,
}

impl VirtualMic {
    /// Create a new virtual microphone with noise reduction.
    ///
    /// # Arguments
    /// * `state` - Application state
    /// * `source_device_id` - PipeWire node ID of the source device
    /// * `virtual_name` - Name for the virtual microphone
    /// * `input_channels` - Number of channels on the input device (1=mono, 2=stereo, etc.)
    ///
    /// TODO: Validate behavior with mono input devices (USB mics, built-in laptop mics)
    /// TODO: Validate behavior with multichannel interfaces (>2 channels)
    pub fn create(
        state: Arc<AppState>,
        source_device_id: u32,
        virtual_name: &str,
        input_channels: u32,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        if !is_valid_mic_name(virtual_name) {
            return Err(format!("Invalid mic name: {:?}", virtual_name).into());
        }
        let processor = Arc::new(AudioProcessor::new(state.clone()));

        let source_name = virtual_name.to_string();
        let sink = sink_name(&source_name);
        let display_name = virtual_name.to_string();

        let mut pulse_ctx = PulseContext::new("spectral")?;

        let sink_args = format!(
            "sink_name={} sink_properties=device.description=Spectral-DoNotUse rate=48000 channels=1 format=float32le",
            sink
        );

        let sink_module_id = match pulse_ctx.load_module("module-null-sink", &sink_args) {
            Ok(id) => Some(id),
            Err(e) => {
                return Err(format!("Failed to create null sink: {}", e).into());
            }
        };

        let monitor = monitor_name(&source_name);
        let source_properties = format!(
            "device.description={} node.description={} device.icon_name=audio-input-microphone",
            display_name, display_name
        );

        let remap_args = format!(
            "source_name={} master={} source_properties={} channels=1",
            source_name, monitor, source_properties
        );

        let remap_module_id = match pulse_ctx.load_module("module-remap-source", &remap_args) {
            Ok(id) => Some(id),
            Err(e) => {
                if let Some(sink_id) = sink_module_id {
                    let _ = pulse_ctx.unload_module(sink_id);
                }
                return Err(format!("Failed to create remapped source: {}", e).into());
            }
        };

        if let Err(e) = pulse_ctx.wait_for_source(&source_name) {
            if let Some(remap_id) = remap_module_id {
                let _ = pulse_ctx.unload_module(remap_id);
            }
            if let Some(sink_id) = sink_module_id {
                let _ = pulse_ctx.unload_module(sink_id);
            }
            return Err(format!("Remapped source did not become ready: {}", e).into());
        }

        if let Err(e) = pulse_ctx.set_default_source(&source_name) {
            eprintln!("Warning: Failed to set default source: {}", e);
        }

        let source_node_name = get_node_name(source_device_id)?;

        let audio_pipeline = match NativeAudioPipeline::start(
            processor.clone(),
            &source_node_name,
            &sink,
            input_channels,
        ) {
            Ok(pipeline) => Some(pipeline),
            Err(e) => {
                if let Some(remap_id) = remap_module_id {
                    let _ = pulse_ctx.unload_module(remap_id);
                }
                if let Some(sink_id) = sink_module_id {
                    let _ = pulse_ctx.unload_module(sink_id);
                }
                return Err(format!("Failed to start audio pipeline: {}", e).into());
            }
        };

        Ok(Self {
            _source_name: source_name,
            sink_module_id,
            remap_module_id,
            audio_pipeline,
            _processor: processor,
        })
    }

    pub fn stop(mut self) {
        if let Some(mut pipeline) = self.audio_pipeline.take() {
            pipeline.stop();
        }

        if let Ok(mut ctx) = PulseContext::new("spectral-cleanup") {
            if let Some(id) = self.remap_module_id.take()
                && let Err(e) = ctx.unload_module(id)
            {
                eprintln!("Failed to unload remap module: {}", e);
            }
            if let Some(id) = self.sink_module_id.take()
                && let Err(e) = ctx.unload_module(id)
            {
                eprintln!("Failed to unload sink module: {}", e);
            }
        } else {
            eprintln!("Failed to create PulseContext for cleanup");
        }
    }
}

impl Drop for VirtualMic {
    fn drop(&mut self) {
        // Only clean up if resources haven't been released
        // stop() takes ownership and clears these fields
        if self.audio_pipeline.is_some()
            || self.sink_module_id.is_some()
            || self.remap_module_id.is_some()
        {
            eprintln!("⚠ VirtualMic dropped without explicit stop(), cleaning up...");

            if let Some(mut pipeline) = self.audio_pipeline.take() {
                pipeline.stop();
            }

            if let Ok(mut ctx) = PulseContext::new("spectral-cleanup") {
                if let Some(id) = self.remap_module_id.take() {
                    let _ = ctx.unload_module(id);
                }
                if let Some(id) = self.sink_module_id.take() {
                    let _ = ctx.unload_module(id);
                }
            }
        }
    }
}

/// Connect to PulseAudio and unload any Spectral virtual-mic modules orphaned by
/// a previous daemon instance (crash/SIGKILL bypasses Drop). Best-effort cleanup
/// keyed on the modules' own properties; returns the number unloaded.
pub fn reap_leaked_modules() -> Result<usize, String> {
    PulseContext::new("spectral-reap")?.reap_spectral_modules()
}
