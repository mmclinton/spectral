use super::audio_devices::get_node_name;
use super::pipewire::{NativeAudioPipeline, RNNOISE_FRAME_SIZE};
use super::pulse::PulseContext;
use crate::state::{AppState, is_valid_mic_name, monitor_name, sink_name};
use nnnoiseless::DenoiseState;
use std::sync::Arc;
use std::sync::atomic::Ordering;

/// Maps the 0-100 threshold to a VAD confidence cutoff. 0 disables the gate
/// (denoise only); 1-100 map linearly to a cutoff of up to 0.95 — the VAD
/// sigmoid saturates at 1.0, so a cutoff of 1.0 could never open the gate,
/// and cutoffs above ~0.95 reject genuine speech onsets. Out-of-range input
/// is clamped so a corrupt persisted threshold can never silence the mic.
fn vad_cutoff(threshold: i32) -> Option<f32> {
    let t = threshold.clamp(0, 100);
    (t > 0).then(|| t as f32 / 100.0 * 0.95)
}

pub struct AudioProcessor {
    state: Arc<AppState>,
    denoiser: Box<DenoiseState<'static>>,
    vad_state: f32,
}

impl AudioProcessor {
    pub fn new(state: Arc<AppState>) -> Self {
        Self {
            state,
            denoiser: DenoiseState::new(),
            vad_state: 0.0,
        }
    }

    /// Denoise and gate exactly one frame: `input` is RNNOISE_FRAME_SIZE mono
    /// samples scaled to the i16 range, `output` receives the processed frame.
    /// Runs on the real-time audio thread — no locks, no allocations.
    pub fn process_frame(&mut self, input: &[f32], output: &mut [f32]) {
        debug_assert_eq!(input.len(), RNNOISE_FRAME_SIZE);
        debug_assert_eq!(output.len(), RNNOISE_FRAME_SIZE);

        let threshold = self.state.threshold_val.load(Ordering::Relaxed);

        // RNNoise processes audio in the i16 range [-32768.0, 32767.0] and
        // returns the frame's VAD probability (0.0-1.0).
        let vad_probability = self.denoiser.process_frame(output, input);

        match vad_cutoff(threshold) {
            Some(cutoff) => {
                // Smooth gate transitions: attack=0.50 (~20ms ramp), release=0.96 (~250ms decay)
                if vad_probability > cutoff {
                    self.vad_state = 0.50 * self.vad_state + 0.50;
                } else {
                    self.vad_state *= 0.96;
                }
                for sample in output.iter_mut() {
                    *sample *= self.vad_state;
                }
            }
            None => self.vad_state = 0.0,
        }
    }
}

pub struct VirtualMic {
    sink_module_id: Option<u32>,
    remap_module_id: Option<u32>,
    audio_pipeline: Option<NativeAudioPipeline>,
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
        let processor = AudioProcessor::new(state.clone());

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

        let audio_pipeline =
            match NativeAudioPipeline::start(processor, &source_node_name, &sink, input_channels) {
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
            sink_module_id,
            remap_module_id,
            audio_pipeline,
        })
    }

    /// Why the underlying pipeline died, if it has.
    pub fn failure(&self) -> Option<String> {
        self.audio_pipeline.as_ref()?.failure()
    }

    /// Tear down the mic. The module ids are cleared even on error — Drop
    /// stays a no-op and the reap machinery owns whatever PulseAudio kept.
    pub fn stop(mut self) -> Result<(), String> {
        if let Some(mut pipeline) = self.audio_pipeline.take() {
            pipeline.stop();
        }

        let mut errors = Vec::new();
        match PulseContext::new("spectral-cleanup") {
            Ok(mut ctx) => {
                if let Some(id) = self.remap_module_id.take()
                    && let Err(e) = ctx.unload_module(id)
                {
                    errors.push(format!("failed to unload remap module: {e}"));
                }
                if let Some(id) = self.sink_module_id.take()
                    && let Err(e) = ctx.unload_module(id)
                {
                    errors.push(format!("failed to unload sink module: {e}"));
                }
            }
            Err(e) => {
                self.remap_module_id.take();
                self.sink_module_id.take();
                errors.push(format!("failed to connect to PulseAudio for cleanup: {e}"));
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
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

#[cfg(test)]
mod tests {
    use super::vad_cutoff;

    #[test]
    fn threshold_maps_linearly_onto_the_vad_cutoff() {
        let cases = [
            (-5, None),
            (0, None),
            (1, Some(0.0095)),
            (50, Some(0.475)),
            (80, Some(0.76)),
            (100, Some(0.95)),
            (400, Some(0.95)),
        ];
        for (threshold, expected) in cases {
            let got = vad_cutoff(threshold);
            match (got, expected) {
                (None, None) => {}
                (Some(g), Some(e)) => assert!(
                    (g - e).abs() < 1e-6,
                    "threshold {threshold}: got {g}, expected {e}"
                ),
                _ => panic!("threshold {threshold}: got {got:?}, expected {expected:?}"),
            }
        }
    }
}
