//! Auto level: a slow RMS-tracking gain rider that evens out mic distance
//! and voice-volume swings, with a built-in peak limiter as the safety
//! ceiling. Adaptation is frozen on non-voice frames so silence and noise
//! floors are never pumped up. All levels are in the i16-range float domain
//! the processing chain runs in (full scale 32768).

// Adaptation only tracks frames RNNoise is at least even-odds sure contain
// voice; independent of the user threshold so leveling works with the gate
// at 0.
const VAD_FLOOR: f32 = 0.5;

// 32768 * 10^(-18/20): a -18 dBFS RMS target, the common speech mix level.
const TARGET_RMS: f32 = 4125.3;

// Frames quieter than this (-90 dBFS RMS) are treated as silence rather
// than driven toward the target.
const MIN_RMS: f32 = 1.0;

// 10^(-12/20) and 10^(18/20): the AGC may cut 12 dB or boost 18 dB, never
// more, so a whisper-quiet or hot input cannot run the gain away.
const GAIN_MIN: f32 = 0.251_19;
const GAIN_MAX: f32 = 7.943_3;

// 10^(0.03/20): 0.03 dB per 10 ms frame = 3 dB/s, slow enough to ride
// average level without audible pumping.
const SLEW_PER_FRAME: f32 = 1.003_460_5;

// 32768 * 10^(-1/20): the limiter's -1 dBFS ceiling.
const CEILING: f32 = 29_206.0;

// Per-sample recovery fraction: ~100 ms (4800 samples at 48 kHz) to
// substantially release after a peak.
const LIMITER_RELEASE: f32 = 1.0 / 4800.0;

pub struct Leveler {
    gain: f32,
    limiter_gain: f32,
}

impl Default for Leveler {
    fn default() -> Self {
        Self {
            gain: 1.0,
            limiter_gain: 1.0,
        }
    }
}

impl Leveler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply the current gain to one frame, adapting it one slew step when
    /// the frame is confidently voice, then hold every peak under the
    /// ceiling (instant attack, ~100 ms release).
    pub fn process(&mut self, frame: &mut [f32], vad: f32) {
        if vad > VAD_FLOOR {
            let mean_square = frame.iter().map(|&s| s * s).sum::<f32>() / frame.len().max(1) as f32;
            let rms = mean_square.sqrt();
            if rms > MIN_RMS {
                let desired = (TARGET_RMS / rms).clamp(GAIN_MIN, GAIN_MAX);
                self.gain = if desired > self.gain {
                    (self.gain * SLEW_PER_FRAME).min(desired)
                } else {
                    (self.gain / SLEW_PER_FRAME).max(desired)
                };
            }
        }

        for sample in frame.iter_mut() {
            let amplified = *sample * self.gain;
            let peak = (amplified * self.limiter_gain).abs();
            if peak > CEILING {
                self.limiter_gain *= CEILING / peak;
            } else {
                self.limiter_gain += LIMITER_RELEASE * (1.0 - self.limiter_gain);
            }
            *sample = amplified * self.limiter_gain;
        }
        if self.limiter_gain > 1.0 - 1e-9 {
            self.limiter_gain = 1.0;
        }
    }

    /// Back to neutral — used while the stage is disabled so a re-engage
    /// starts transparent instead of at a stale gain.
    pub fn reset(&mut self) {
        self.gain = 1.0;
        self.limiter_gain = 1.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: usize = 480;

    /// One frame of a 1 kHz sine at 48 kHz (an exact 10 periods, so every
    /// frame has identical RMS = amplitude / sqrt(2)).
    fn sine_frame(amplitude: f32) -> Vec<f32> {
        (0..FRAME)
            .map(|i| amplitude * (std::f64::consts::TAU * i as f64 / 48.0).sin() as f32)
            .collect()
    }

    fn rms(frame: &[f32]) -> f32 {
        (frame.iter().map(|&s| s * s).sum::<f32>() / frame.len() as f32).sqrt()
    }

    fn run_frames(leveler: &mut Leveler, amplitude: f32, vad: f32, count: usize) -> Vec<f32> {
        let mut last = Vec::new();
        for _ in 0..count {
            let mut frame = sine_frame(amplitude);
            leveler.process(&mut frame, vad);
            last = frame;
        }
        last
    }

    #[test]
    fn quiet_voice_converges_to_the_target_rms() {
        // -30 dBFS input needs +12 dB: 400 frames at 0.03 dB/frame.
        let mut leveler = Leveler::new();
        let amplitude = 32768.0 * 0.031_623 * std::f32::consts::SQRT_2; // RMS -30 dBFS
        let last = run_frames(&mut leveler, amplitude, 1.0, 600);
        let ratio = rms(&last) / TARGET_RMS;
        assert!((0.94..1.06).contains(&ratio), "ratio {ratio}");
    }

    #[test]
    fn adaptation_freezes_without_voice() {
        let mut leveler = Leveler::new();
        let amplitude = 32768.0 * 0.031_623 * std::f32::consts::SQRT_2;
        let mid = run_frames(&mut leveler, amplitude, 1.0, 100);
        let frozen = run_frames(&mut leveler, amplitude, 0.0, 200);
        let drift = rms(&frozen) / rms(&mid);
        assert!((0.999..1.001).contains(&drift), "drift {drift}");
    }

    #[test]
    fn boost_is_clamped_at_18_db() {
        // -60 dBFS input would want +42 dB; the clamp holds it at +18 dB.
        let mut leveler = Leveler::new();
        let amplitude = 32.768 * std::f32::consts::SQRT_2; // RMS -60 dBFS
        let last = run_frames(&mut leveler, amplitude, 1.0, 800);
        let gain = rms(&last) / (amplitude / std::f32::consts::SQRT_2);
        assert!(
            (GAIN_MAX * 0.98..GAIN_MAX * 1.02).contains(&gain),
            "gain {gain}"
        );
    }

    #[test]
    fn limiter_holds_peaks_under_the_ceiling() {
        // Full-scale input with the AGC still at unity (vad 0 keeps it
        // frozen): raw peaks would hit 32768, 12% over the ceiling.
        let mut leveler = Leveler::new();
        let last = run_frames(&mut leveler, 32768.0, 0.0, 5);
        let peak = last.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
        assert!(peak <= CEILING * 1.001, "peak {peak}");
    }

    #[test]
    fn reset_returns_to_neutral() {
        let mut leveler = Leveler::new();
        run_frames(&mut leveler, 100.0, 1.0, 200);
        leveler.reset();
        let mut frame = sine_frame(1000.0);
        let before = rms(&frame);
        leveler.process(&mut frame, 0.0);
        let after = rms(&frame);
        assert!((after / before - 1.0).abs() < 1e-3);
    }
}
