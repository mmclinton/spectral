use super::pipewire::NativeAudioPipeline;
use super::pulse::PulseContext;
use crate::state::AppState;
use nnnoiseless::DenoiseState;
use std::process::Command;
use std::sync::{Arc, Mutex};

// Audio processing constants for nnnoiseless RNNoise implementation
const FRAME_SIZE: usize = 480; // RNNoise processes 10ms frames at 48kHz (480 samples)

// IMPORTANT: RNNoise expects samples in i16 range [-32768.0, 32767.0] as f32 values
// NOT the typical float32 PCM range of [-1.0, 1.0]
// Conversion is handled in pipewire_native.rs before/after RNNoise processing

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
                input_buffer: Vec::with_capacity(FRAME_SIZE * 2),
                vad_state: 0.0,
                scratch_input: vec![0.0; FRAME_SIZE],
                scratch_output: vec![0.0; FRAME_SIZE],
            }),
        }
    }

    /// Called from PipeWire stream callbacks for real-time audio processing
    pub fn process_audio_buffer(&self, input: &[f32], output: &mut [f32]) {
        let threshold = *self
            .state
            .threshold_val
            .lock()
            .expect("threshold_val mutex poisoned in audio processing")
            as f32
            / 100.0;

        let mut ctx_guard = self
            .context
            .lock()
            .expect("AudioProcessor context mutex poisoned");

        let ProcessingContext {
            denoiser,
            input_buffer,
            vad_state,
            scratch_input,
            scratch_output,
        } = &mut *ctx_guard;

        input_buffer.extend_from_slice(input);

        let mut out_idx = 0;

        // Process in FRAME_SIZE chunks (480 samples = 10ms at 48kHz)
        while input_buffer.len() >= FRAME_SIZE {
            // Move data to scratch_input without allocating a new Vec
            for (i, sample) in input_buffer.drain(..FRAME_SIZE).enumerate() {
                scratch_input[i] = sample;
            }

            // Apply RNNoise - processes audio in i16 range [-32768.0, 32767.0]
            // Returns VAD probability (0.0-1.0) indicating voice presence confidence
            let vad_probability = denoiser.process_frame(scratch_output, scratch_input);

            if threshold >= 0.5 {
                // Map threshold 50-100% to VAD cutoff 0.5-0.95
                let vad_threshold = (threshold - 0.5) * 0.9 + 0.5;

                // Determine gate state from VAD probability
                let target_gate = if vad_probability > vad_threshold {
                    1.0
                } else {
                    0.0
                };

                // Smooth gate transitions to prevent clicks and word cutoff
                // These coefficients create fade-in/fade-out instead of hard cuts
                // Attack: 0.85 = ~75ms ramp up (fast enough to catch word start)
                // Release: 0.96 = ~250ms ramp down (keeps tail of words/sentences)
                let attack_coef = 0.85; // Faster attack to catch word beginning
                let release_coef = 0.96; // Faster release than previous (0.98) - closes gate quicker after voice stops

                if target_gate > 0.5 {
                    // Voice detected - ramp up gate state
                    *vad_state = attack_coef * (*vad_state) + (1.0 - attack_coef);
                } else {
                    // No voice - ramp down gate state
                    *vad_state *= release_coef;
                }

                // Apply smooth gain instead of binary gate
                // This should create a more natural fade-in/fade-out transitions
                // vad_state ranges from 0.0 (silent) to 1.0 (full volume)
                let gain = *vad_state;

                // Apply the smooth gain to create gradual transitions
                for &sample in scratch_output.iter() {
                    if out_idx < output.len() {
                        output[out_idx] = sample * gain;
                        out_idx += 1;
                    }
                }
            } else {
                // Low threshold: passthrough denoised audio without gating
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
    pub fn create(
        state: Arc<AppState>,
        source_device_id: u32,
        virtual_name: &str,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let processor = Arc::new(AudioProcessor::new(state.clone()));

        let source_name = virtual_name.replace(" ", "_");
        let sink_name = format!("{}_sink", source_name);
        let display_name = virtual_name.replace(" ", "-");

        // Create PulseAudio context
        let mut pulse_ctx = PulseContext::new("spectral")?;

        // Load null sink module
        let sink_args = format!(
            "sink_name={} sink_properties=device.description=Spectral-DoNotUse rate=48000 channels=1 format=float32le",
            sink_name
        );

        let sink_module_id = match pulse_ctx.load_module("module-null-sink", &sink_args) {
            Ok(id) => Some(id),
            Err(e) => {
                return Err(format!("Failed to create null sink: {}", e).into());
            }
        };

        let monitor_name = format!("{}.monitor", sink_name);
        let source_properties = format!(
            "device.description={} node.description={} device.icon_name=audio-input-microphone",
            display_name, display_name
        );

        // Load remap source module
        let remap_args = format!(
            "source_name={} master={} source_properties={} channels=1",
            source_name, monitor_name, source_properties
        );

        let remap_module_id = match pulse_ctx.load_module("module-remap-source", &remap_args) {
            Ok(id) => Some(id),
            Err(e) => {
                // Cleanup sink module on failure
                if let Some(sink_id) = sink_module_id {
                    let _ = pulse_ctx.unload_module(sink_id);
                }
                return Err(format!("Failed to create remapped source: {}", e).into());
            }
        };

        // Give PipeWire a moment to register the new node
        std::thread::sleep(std::time::Duration::from_millis(200));

        let source_node_name = Self::get_node_name(source_device_id)?;

        let audio_pipeline =
            match NativeAudioPipeline::start(processor.clone(), &source_node_name, &sink_name) {
                Ok(pipeline) => Some(pipeline),
                Err(e) => {
                    eprintln!("Failed to create audio pipeline: {}", e);
                    None
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

    fn get_node_name(node_id: u32) -> Result<String, Box<dyn std::error::Error>> {
        let output = Command::new("wpctl")
            .args(["inspect", &node_id.to_string()])
            .output()?;

        if !output.status.success() {
            return Err(format!("Failed to inspect node {}", node_id).into());
        }

        let output_str = String::from_utf8_lossy(&output.stdout);

        for line in output_str.lines() {
            if line.contains("node.name") {
                // Line format: "    node.name = "alsa_input.usb-...""
                if let Some(name_part) = line.split('"').nth(1) {
                    return Ok(name_part.to_string());
                }
            }
        }

        Err(format!("Could not find node.name for node {}", node_id).into())
    }

    pub fn stop(mut self) {
        if let Some(mut pipeline) = self.audio_pipeline.take() {
            pipeline.stop();
        }

        // Create a new PulseContext just for cleanup
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
