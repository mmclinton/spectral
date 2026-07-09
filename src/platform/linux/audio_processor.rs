use super::audio_devices::{get_node_name, set_default_source};
use super::pipewire::{NativeAudioPipeline, RNNOISE_FRAME_SIZE};
use super::stages::GateEnvelope;
use crate::state::{AppState, is_valid_mic_name};
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
    gate: GateEnvelope,
}

impl AudioProcessor {
    pub fn new(state: Arc<AppState>) -> Self {
        Self {
            state,
            denoiser: DenoiseState::new(),
            gate: GateEnvelope::new(),
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
            Some(cutoff) => self.gate.process(output, vad_probability > cutoff),
            None => self.gate.reset(),
        }
    }
}

pub struct VirtualMic {
    audio_pipeline: Option<NativeAudioPipeline>,
}

impl VirtualMic {
    /// Create a new virtual microphone with noise reduction. The pipeline
    /// thread owns every PipeWire object involved (mic node, streams, feed
    /// link), so the mic disappears with the daemon by construction.
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
        let source_node_name = get_node_name(source_device_id)?;

        let audio_pipeline =
            NativeAudioPipeline::start(processor, &source_node_name, virtual_name, input_channels)
                .map_err(|e| format!("Failed to start audio pipeline: {}", e))?;

        // The mic node exists once the pipeline is ready, so the configured
        // default targets a live name. Best-effort: never fails an enable.
        if let Err(e) = set_default_source(virtual_name) {
            eprintln!("Warning: Failed to set default source: {}", e);
        }

        Ok(Self {
            audio_pipeline: Some(audio_pipeline),
        })
    }

    /// Why the underlying pipeline died, if it has.
    pub fn failure(&self) -> Option<String> {
        self.audio_pipeline.as_ref()?.failure()
    }

    /// Tear down the mic: stopping the pipeline thread drops the PipeWire
    /// connection, which destroys the mic node, streams, and feed link.
    pub fn stop(mut self) {
        if let Some(mut pipeline) = self.audio_pipeline.take() {
            pipeline.stop();
        }
    }
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
