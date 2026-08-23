//! The per-frame processing chain (high-pass → RNNoise → leveler → gate)
//! and the [`VirtualMic`] that owns the pipeline running it.

use super::audio_devices::set_default_source;
use super::pipewire::{NativeAudioPipeline, RNNOISE_FRAME_SIZE, SAMPLE_RATE};
use super::stages::{HighPass, Leveler, VoiceGate};
use crate::state::{AppState, is_valid_mic_name, mic_name_rule};
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

/// Below typical voice fundamentals but above mains hum's second harmonic
/// region; deep rumble goes, voices keep their body.
const RUMBLE_CUTOFF_HZ: f64 = 90.0;

/// The complete per-frame DSP chain, owned by the pipeline thread. Reads
/// its settings from [`AppState`] atomics each frame, so toggles apply live
/// without locking the audio path.
pub struct AudioProcessor {
    state: Arc<AppState>,
    denoiser: Box<DenoiseState<'static>>,
    highpass: HighPass,
    leveler: Leveler,
    gate: VoiceGate,
}

impl AudioProcessor {
    /// Builds the chain with fresh stage state; allocation happens here,
    /// never in [`AudioProcessor::process_frame`].
    pub fn new(state: Arc<AppState>) -> Self {
        Self {
            state,
            denoiser: DenoiseState::new(),
            highpass: HighPass::new(RUMBLE_CUTOFF_HZ, SAMPLE_RATE as f64, RNNOISE_FRAME_SIZE),
            leveler: Leveler::new(),
            gate: VoiceGate::new(RNNOISE_FRAME_SIZE),
        }
    }

    /// Denoise and gate exactly one frame: `input` is RNNOISE_FRAME_SIZE mono
    /// samples scaled to the i16 range, `output` receives the processed frame.
    /// Runs on the real-time audio thread — no locks, no allocations.
    pub fn process_frame(&mut self, input: &[f32], output: &mut [f32]) {
        debug_assert_eq!(input.len(), RNNOISE_FRAME_SIZE);
        debug_assert_eq!(output.len(), RNNOISE_FRAME_SIZE);

        let threshold = self.state.threshold_val.load(Ordering::Relaxed);

        // Pre-denoise so RNNoise and its VAD analyze a rumble-free signal.
        let input = if self.state.rumble_filter.load(Ordering::Relaxed) {
            self.highpass.process(input)
        } else {
            self.highpass.reset();
            input
        };

        // RNNoise processes audio in the i16 range [-32768.0, 32767.0] and
        // returns the frame's VAD probability (0.0-1.0).
        let vad_probability = self.denoiser.process_frame(output, input);

        if self.state.auto_level.load(Ordering::Relaxed) {
            self.leveler.process(output, vad_probability);
        } else {
            self.leveler.reset();
        }

        // The gate delays the output by LOOKAHEAD_FRAMES (constant, also in
        // denoise-only bypass) so its decisions lead the audio.
        self.gate
            .process(output, vad_probability, vad_cutoff(threshold));
    }
}

/// The daemon's handle on the virtual microphone; `stop` (or drop) ends the
/// pipeline thread, which removes the mic from the graph.
pub struct VirtualMic {
    audio_pipeline: Option<NativeAudioPipeline>,
    node_name: String,
}

impl VirtualMic {
    /// Create a new virtual microphone named `virtual_name` with noise
    /// reduction, capturing `source_node_name` with `input_channels`
    /// channels — both already resolved by the caller in one registry pass.
    /// The pipeline thread owns every PipeWire object involved (mic node,
    /// streams, feed link), so the mic disappears with the daemon by
    /// construction.
    ///
    /// Channel handling is validated by listening on mono and stereo
    /// capture; layouts above two channels negotiate their native count up
    /// to the ceiling and are untested.
    pub fn create(
        state: Arc<AppState>,
        source_node_name: &str,
        virtual_name: &str,
        input_channels: u32,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        if !is_valid_mic_name(virtual_name) {
            let rule = mic_name_rule();
            return Err(format!("Invalid mic name {virtual_name:?}: {rule}").into());
        }
        let processor = AudioProcessor::new(state.clone());

        let audio_pipeline =
            NativeAudioPipeline::start(processor, source_node_name, virtual_name, input_channels)
                .map_err(|e| format!("Failed to start audio pipeline: {e}"))?;

        // The mic node exists once the pipeline is ready, so the configured
        // default targets a live name. Best-effort: never fails an enable.
        if let Err(e) = set_default_source(virtual_name) {
            eprintln!("[daemon] warning: failed to set default source: {e}");
        }

        Ok(Self {
            audio_pipeline: Some(audio_pipeline),
            node_name: virtual_name.to_string(),
        })
    }

    /// The PipeWire node name this mic was created with — the name a
    /// recording stream must target. `state.mic_name` can already hold a
    /// newer, not-yet-applied name while this mic is still the live one.
    pub fn node_name(&self) -> &str {
        &self.node_name
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
    use super::{AudioProcessor, vad_cutoff};
    use crate::platform::linux::pipewire::{RNNOISE_FRAME_SIZE, SAMPLE_RATE};
    use crate::platform::linux::stages::gate::LOOKAHEAD_FRAMES;
    use crate::state::AppState;
    use std::sync::Arc;
    use std::sync::atomic::Ordering;

    fn sine(freq_hz: f64, amplitude: f32, frame_idx: usize) -> Vec<f32> {
        (0..RNNOISE_FRAME_SIZE)
            .map(|i| {
                let n = (frame_idx * RNNOISE_FRAME_SIZE + i) as f64;
                (std::f64::consts::TAU * freq_hz * n / f64::from(SAMPLE_RATE)).sin() as f32
                    * amplitude
            })
            .collect()
    }

    fn frame_rms(frame: &[f32]) -> f64 {
        (frame
            .iter()
            .map(|&s| f64::from(s) * f64::from(s))
            .sum::<f64>()
            / frame.len().max(1) as f64)
            .sqrt()
    }

    /// Deterministic white noise in the i16 range RNNoise works in. A fixed
    /// LCG keeps runs comparable without pulling in a PRNG crate.
    fn noise(amplitude: f32, seed: &mut u32) -> Vec<f32> {
        (0..RNNOISE_FRAME_SIZE)
            .map(|_| {
                *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let unit = f64::from(*seed >> 8) / f64::from(1u32 << 24) * 2.0 - 1.0;
                unit as f32 * amplitude
            })
            .collect()
    }

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

    /// The pipeline's latency, which no setting may shift. A marked input
    /// frame's leading edge re-emerges exactly [`LOOKAHEAD_FRAMES`] calls
    /// later — the gate's delay line, whose first [`LOOKAHEAD_FRAMES`]
    /// outputs are its priming zeros — and the frame's energy peaks one
    /// call after that, which is RNNoise's own 10 ms algorithmic delay.
    /// The two together are the 10 ms and the 20 ms `docs/ARCHITECTURE.md`
    /// documents; pinning them apart means a change in either names the
    /// stage that moved. An engaged threshold and the bypass both run,
    /// because the delay must not follow the settings.
    ///
    /// The impulse is loud enough that RNNoise's VAD calls it voice and the
    /// lookahead pre-opens the gate, so both runs come out through an open
    /// gate — the closed gate is
    /// [`an_engaged_gate_attenuates_noise_the_bypass_lets_through`]'s job.
    #[test]
    fn the_chain_delay_is_the_gate_lookahead_plus_the_denoiser_frame_at_any_threshold() {
        const LEAD_IN: usize = 10;
        for threshold in [50, 0] {
            let state = Arc::new(AppState::new());
            state.threshold_val.store(threshold, Ordering::Relaxed);
            let mut processor = AudioProcessor::new(state);
            let mut outputs = Vec::new();
            for frame_idx in 0..LEAD_IN + LOOKAHEAD_FRAMES + 3 {
                let input = if frame_idx == LEAD_IN {
                    vec![30_000.0; RNNOISE_FRAME_SIZE]
                } else {
                    vec![0.0; RNNOISE_FRAME_SIZE]
                };
                let mut output = vec![0.0; RNNOISE_FRAME_SIZE];
                processor.process_frame(&input, &mut output);
                outputs.push(output);
            }

            for (i, frame) in outputs.iter().take(LOOKAHEAD_FRAMES).enumerate() {
                assert!(
                    frame.iter().all(|&s| s == 0.0),
                    "threshold {threshold}: priming frame {i} is not exact zeros"
                );
            }
            let peaks: Vec<f32> = outputs
                .iter()
                .map(|f| f.iter().fold(0.0f32, |m, &s| m.max(s.abs())))
                .collect();
            // The floor comes off the post-priming, pre-impulse frames.
            // Digital silence has measured as exact zeros through the
            // denoiser, so the epsilon only guards a future one that leaks.
            let floor = peaks[LOOKAHEAD_FRAMES..LEAD_IN]
                .iter()
                .fold(0.0f32, |m, &p| m.max(p));
            assert_eq!(
                peaks.iter().position(|&p| p > floor * 20.0 + 1.0),
                Some(LEAD_IN + LOOKAHEAD_FRAMES),
                "threshold {threshold}: the leading edge did not emerge one \
                 lookahead after the impulse — the gate's delay depth moved"
            );
            assert_eq!(
                peaks
                    .iter()
                    .enumerate()
                    .max_by(|(_, a), (_, b)| a.total_cmp(b))
                    .map(|(i, _)| i),
                Some(LEAD_IN + LOOKAHEAD_FRAMES + 1),
                "threshold {threshold}: the energy did not peak one frame after \
                 the leading edge — the denoiser's algorithmic delay moved"
            );
        }
    }

    /// The threshold atomic has to reach the gate in the live chain: the
    /// same noise comes out far quieter with the gate engaged than with it
    /// bypassed. Relative, so the claim holds however much RNNoise itself
    /// attenuates noise; the gate's attenuation floor is pinned in
    /// `stages::gate` and not restated here.
    ///
    /// The amplitude is calibrated. At 100 the VAD stays under the
    /// threshold-50 cutoff for every frame — the measured ratio is exactly
    /// the gate's floor across seeds and run lengths — and the VAD does not
    /// start opening the gate intermittently until around 300.
    #[test]
    fn an_engaged_gate_attenuates_noise_the_bypass_lets_through() {
        let run = |threshold: i32| -> f64 {
            let state = Arc::new(AppState::new());
            state.threshold_val.store(threshold, Ordering::Relaxed);
            let mut processor = AudioProcessor::new(state);
            let mut seed = 0x5eed_1234_u32;
            let frames = 200;
            let (mut sum_squares, mut count) = (0.0f64, 0usize);
            for frame_idx in 0..frames {
                let input = noise(100.0, &mut seed);
                let mut output = vec![0.0; RNNOISE_FRAME_SIZE];
                processor.process_frame(&input, &mut output);
                if frame_idx >= frames / 2 {
                    let rms = frame_rms(&output);
                    sum_squares += rms * rms;
                    count += 1;
                }
            }
            (sum_squares / count as f64).sqrt()
        };

        let gated = run(50);
        let bypassed = run(0);
        assert!(
            gated * 5.0 < bypassed,
            "gated rms {gated} is not far below the bypass's {bypassed}"
        );
    }

    /// Flipping the rumble-filter atomic has to change what comes out — the
    /// high-pass sits in the live chain ahead of the denoiser. Two runs of
    /// identical 20 Hz input differ only in that atomic; asserting their RMS
    /// ratio keeps the claim independent of how much RNNoise itself
    /// attenuates rumble. Threshold 0 takes the gate out of both runs.
    #[test]
    fn the_rumble_filter_atomic_attenuates_rumble_in_the_live_chain() {
        let run = |enabled: bool| -> f64 {
            let state = Arc::new(AppState::new());
            state.threshold_val.store(0, Ordering::Relaxed);
            state.rumble_filter.store(enabled, Ordering::Relaxed);
            let mut processor = AudioProcessor::new(state);
            let frames = 200;
            let (mut sum_squares, mut count) = (0.0f64, 0usize);
            for frame_idx in 0..frames {
                let input = sine(20.0, 8000.0, frame_idx);
                let mut output = vec![0.0; RNNOISE_FRAME_SIZE];
                processor.process_frame(&input, &mut output);
                if frame_idx >= frames / 2 {
                    let rms = frame_rms(&output);
                    sum_squares += rms * rms;
                    count += 1;
                }
            }
            (sum_squares / count as f64).sqrt()
        };

        let filtered = run(true);
        let unfiltered = run(false);
        assert!(
            filtered * 2.0 < unfiltered,
            "filtered rms {filtered} is not far below unfiltered {unfiltered}"
        );
    }

    /// Flipping the auto-level atomic has to engage the AGC in the live
    /// chain: repeated quiet input keeps getting louder while it is on and
    /// comes out level while it is off. A 1 kHz tone keeps the real VAD
    /// confidently voiced (measured mean ~0.99), so the stage's vad-freeze
    /// branch stays out of the comparison; threshold 0 bypasses the gate.
    #[test]
    fn the_auto_level_atomic_engages_the_agc_in_the_live_chain() {
        let growth = |enabled: bool| -> f64 {
            let state = Arc::new(AppState::new());
            state.threshold_val.store(0, Ordering::Relaxed);
            state.auto_level.store(enabled, Ordering::Relaxed);
            let mut processor = AudioProcessor::new(state);
            let frames = 400;
            let (mut early_sum, mut late_sum) = (0.0f64, 0.0f64);
            for frame_idx in 0..frames {
                let input = sine(1000.0, 2000.0, frame_idx);
                let mut output = vec![0.0; RNNOISE_FRAME_SIZE];
                processor.process_frame(&input, &mut output);
                if frame_idx < 50 {
                    early_sum += frame_rms(&output);
                } else {
                    late_sum += frame_rms(&output);
                }
            }
            (late_sum / (frames - 50) as f64) / (early_sum / 50.0)
        };

        let growing = growth(true);
        assert!(
            growing > 1.5,
            "agc on: late/early rms ratio {growing} shows no ride"
        );
        let flat = growth(false);
        assert!(flat < 1.15, "agc off: late/early rms ratio {flat} drifted");
    }
}
