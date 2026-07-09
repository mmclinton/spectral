//! Voice-gate envelope: turns the per-frame binary gate decision into a
//! click-free gain ramp.

// Below this the release is inaudible; snapping to zero keeps the one-pole
// from decaying through denormal range forever.
const GAIN_FLOOR: f32 = 1e-9;

/// Gate character: one-pole attack/release coefficients per 10 ms frame.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GatePreset {
    /// Full open in one frame, ~100 ms to silence — perceptually "just the
    /// threshold", no audible transformation beyond the gating itself.
    Tight,
    /// Soft ~20-40 ms attack and a ~0.5 s release tail — the "voice
    /// smoothing" character.
    Gentle,
}

impl GatePreset {
    fn coeffs(self) -> (f32, f32) {
        match self {
            GatePreset::Tight => (0.0, 0.5),
            GatePreset::Gentle => (0.5, 0.96),
        }
    }
}

#[derive(Default)]
pub struct GateEnvelope {
    gain: f32,
}

impl GateEnvelope {
    pub fn new() -> Self {
        Self::default()
    }

    /// Advance one attack/release step toward `open` and apply the gain,
    /// interpolated sample-by-sample from the previous frame's value so a
    /// gate transition never steps at a frame boundary.
    pub fn process(&mut self, frame: &mut [f32], open: bool, preset: GatePreset) {
        let (attack, release) = preset.coeffs();
        let start = self.gain;
        let mut target = if open {
            attack * start + (1.0 - attack)
        } else {
            release * start
        };
        if target < GAIN_FLOOR {
            target = 0.0;
        }
        let step = (target - start) / frame.len() as f32;
        for (i, sample) in frame.iter_mut().enumerate() {
            *sample *= start + step * (i + 1) as f32;
        }
        self.gain = target;
    }

    /// Snap shut with no tail — used while the gate is disabled
    /// (denoise-only) so a later re-engage starts from silence.
    pub fn reset(&mut self) {
        self.gain = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: usize = 8;

    fn run(env: &mut GateEnvelope, open: bool, preset: GatePreset) -> Vec<f32> {
        let mut frame = vec![1.0f32; FRAME];
        env.process(&mut frame, open, preset);
        frame
    }

    fn end_gain(env: &mut GateEnvelope, open: bool, preset: GatePreset) -> f32 {
        *run(env, open, preset).last().unwrap()
    }

    #[test]
    fn gain_follows_the_attack_and_release_curves_of_each_preset() {
        for preset in [GatePreset::Tight, GatePreset::Gentle] {
            let (attack, release) = preset.coeffs();
            let mut env = GateEnvelope::new();
            let mut expected = 0.0f32;
            for _ in 0..3 {
                expected = attack * expected + (1.0 - attack);
                assert!(
                    (end_gain(&mut env, true, preset) - expected).abs() < 1e-6,
                    "{preset:?} attack"
                );
            }
            for _ in 0..3 {
                expected *= release;
                assert!(
                    (end_gain(&mut env, false, preset) - expected).abs() < 1e-6,
                    "{preset:?} release"
                );
            }
        }
    }

    #[test]
    fn tight_preset_opens_fully_in_one_frame() {
        let mut env = GateEnvelope::new();
        assert_eq!(end_gain(&mut env, true, GatePreset::Tight), 1.0);
    }

    #[test]
    fn gain_ramps_linearly_within_a_frame() {
        let mut env = GateEnvelope::new();
        let frame = run(&mut env, true, GatePreset::Gentle);
        for (i, sample) in frame.iter().enumerate() {
            let expected = 0.5 * (i + 1) as f32 / FRAME as f32;
            assert!(
                (sample - expected).abs() < 1e-6,
                "sample {i}: got {sample}, expected {expected}"
            );
        }
    }

    #[test]
    fn long_release_settles_at_exact_zero() {
        for preset in [GatePreset::Tight, GatePreset::Gentle] {
            let mut env = GateEnvelope::new();
            run(&mut env, true, preset);
            for _ in 0..1000 {
                run(&mut env, false, preset);
            }
            assert_eq!(end_gain(&mut env, false, preset), 0.0, "{preset:?}");
        }
    }

    #[test]
    fn reset_snaps_shut() {
        let mut env = GateEnvelope::new();
        run(&mut env, true, GatePreset::Gentle);
        env.reset();
        assert_eq!(end_gain(&mut env, false, GatePreset::Gentle), 0.0);
    }
}
