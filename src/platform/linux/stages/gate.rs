//! Voice-gate envelope: turns the per-frame binary gate decision into a
//! click-free gain ramp.

// One-pole coefficients per 10 ms frame: attack reaches perceptually full
// gain in ~2-4 frames; release leaves a ~0.5 s audible tail.
const ATTACK: f32 = 0.50;
const RELEASE: f32 = 0.96;

// Below this the release is inaudible; snapping to zero keeps the one-pole
// from decaying through denormal range forever.
const GAIN_FLOOR: f32 = 1e-9;

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
    pub fn process(&mut self, frame: &mut [f32], open: bool) {
        let start = self.gain;
        let mut target = if open {
            ATTACK * start + (1.0 - ATTACK)
        } else {
            RELEASE * start
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

    fn run(env: &mut GateEnvelope, open: bool) -> Vec<f32> {
        let mut frame = vec![1.0f32; FRAME];
        env.process(&mut frame, open);
        frame
    }

    fn end_gain(env: &mut GateEnvelope, open: bool) -> f32 {
        *run(env, open).last().unwrap()
    }

    #[test]
    fn gain_follows_the_attack_and_release_curves() {
        let mut env = GateEnvelope::new();
        let mut expected = 0.0f32;
        for _ in 0..3 {
            expected = ATTACK * expected + (1.0 - ATTACK);
            assert!((end_gain(&mut env, true) - expected).abs() < 1e-6);
        }
        for _ in 0..3 {
            expected *= RELEASE;
            assert!((end_gain(&mut env, false) - expected).abs() < 1e-6);
        }
    }

    #[test]
    fn gain_ramps_linearly_within_a_frame() {
        let mut env = GateEnvelope::new();
        let frame = run(&mut env, true);
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
        let mut env = GateEnvelope::new();
        run(&mut env, true);
        for _ in 0..1000 {
            run(&mut env, false);
        }
        assert_eq!(end_gain(&mut env, false), 0.0);
    }

    #[test]
    fn reset_snaps_shut() {
        let mut env = GateEnvelope::new();
        run(&mut env, true);
        env.reset();
        assert_eq!(end_gain(&mut env, false), 0.0);
    }
}
