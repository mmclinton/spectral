//! Rumble filter: a 2nd-order Butterworth high-pass biquad (RBJ Audio EQ
//! Cookbook coefficients) that cuts desk thumps, handling noise, and HVAC
//! rumble before the denoiser analyzes the frame.

// Filter state this far below the i16-range signal floor is inaudible;
// snapping to zero keeps the recursion out of denormal range.
const STATE_FLOOR: f32 = 1e-9;

/// One biquad's coefficients and recursion state, with a reusable output
/// buffer so processing stays allocation-free.
pub struct HighPass {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    s1: f32,
    s2: f32,
    buf: Vec<f32>,
}

impl HighPass {
    /// Coefficients are computed once in f64 and fixed for the given rate;
    /// `frame_len` sizes the output buffer so the audio path never allocates.
    pub fn new(cutoff_hz: f64, sample_rate: f64, frame_len: usize) -> Self {
        let w0 = std::f64::consts::TAU * cutoff_hz / sample_rate;
        let (sin_w0, cos_w0) = w0.sin_cos();
        // Q = 1/sqrt(2): the Butterworth (maximally flat) alignment.
        let alpha = sin_w0 / (2.0 * std::f64::consts::FRAC_1_SQRT_2);
        let a0 = 1.0 + alpha;
        Self {
            b0: ((1.0 + cos_w0) / 2.0 / a0) as f32,
            b1: (-(1.0 + cos_w0) / a0) as f32,
            b2: ((1.0 + cos_w0) / 2.0 / a0) as f32,
            a1: (-2.0 * cos_w0 / a0) as f32,
            a2: ((1.0 - alpha) / a0) as f32,
            s1: 0.0,
            s2: 0.0,
            buf: vec![0.0; frame_len],
        }
    }

    /// Filter one frame into the internal buffer (transposed direct form II)
    /// and return it.
    pub fn process(&mut self, input: &[f32]) -> &[f32] {
        debug_assert!(input.len() <= self.buf.len());
        for (x, out) in input.iter().zip(self.buf.iter_mut()) {
            let y = self.b0 * x + self.s1;
            self.s1 = self.b1 * x - self.a1 * y + self.s2;
            self.s2 = self.b2 * x - self.a2 * y;
            *out = y;
        }
        // `!is_finite()` is not redundant with the denormal floor: NaN
        // compares false against every bound, so without it the recursion
        // would keep multiplying NaN forever instead of snapping to zero.
        // The capture path sanitizes its input, but this stage also runs
        // ahead of RNNoise on buffers it does not own.
        if !self.s1.is_finite() || self.s1.abs() < STATE_FLOOR {
            self.s1 = 0.0;
        }
        if !self.s2.is_finite() || self.s2.abs() < STATE_FLOOR {
            self.s2 = 0.0;
        }
        &self.buf[..input.len()]
    }

    /// Clear the recursion so a re-engage starts from silence.
    pub fn reset(&mut self) {
        self.s1 = 0.0;
        self.s2 = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: usize = 480;
    const RATE: f64 = 48000.0;
    const CUTOFF: f64 = 90.0;

    /// Steady-state gain: feed a unit sine and compare RMS over the second
    /// half of a two-second run, after the transient has died.
    fn measured_gain(freq: f64) -> f64 {
        let mut filter = HighPass::new(CUTOFF, RATE, FRAME);
        let frames = 200;
        let mut sum_squares = 0.0f64;
        let mut count = 0usize;
        for frame_idx in 0..frames {
            let frame: Vec<f32> = (0..FRAME)
                .map(|i| {
                    let n = (frame_idx * FRAME + i) as f64;
                    (std::f64::consts::TAU * freq * n / RATE).sin() as f32
                })
                .collect();
            let out = filter.process(&frame);
            if frame_idx >= frames / 2 {
                sum_squares += out.iter().map(|&s| (s as f64).powi(2)).sum::<f64>();
                count += FRAME;
            }
        }
        let out_rms = (sum_squares / count as f64).sqrt();
        out_rms / std::f64::consts::FRAC_1_SQRT_2
    }

    #[test]
    fn deep_rumble_is_strongly_attenuated() {
        // 2nd-order Butterworth at 90 Hz: |H(20 Hz)| ≈ 0.049 (−26 dB).
        assert!(measured_gain(20.0) < 0.08);
    }

    #[test]
    fn mains_hum_region_is_meaningfully_attenuated() {
        // |H(50 Hz)| ≈ 0.295 (−10.6 dB).
        let gain = measured_gain(50.0);
        assert!((0.2..0.4).contains(&gain), "gain {gain}");
    }

    #[test]
    fn voice_band_passes_at_unity() {
        let gain = measured_gain(1000.0);
        assert!((0.99..1.01).contains(&gain), "gain {gain}");
    }

    #[test]
    fn silence_settles_at_exact_zero() {
        let mut filter = HighPass::new(CUTOFF, RATE, FRAME);
        let mut impulse = vec![0.0f32; FRAME];
        impulse[0] = 32768.0;
        filter.process(&impulse);
        let zeros = vec![0.0f32; FRAME];
        let mut last = Vec::new();
        for _ in 0..1000 {
            last = filter.process(&zeros).to_vec();
        }
        assert!(last.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn a_non_finite_sample_does_not_latch_the_recursion() {
        for poison in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let mut filter = HighPass::new(CUTOFF, RATE, FRAME);
            let mut bad = vec![0.0f32; FRAME];
            bad[0] = poison;
            filter.process(&bad);

            // 500 frames is ~5 s at 48 kHz — far past the point where a
            // latched state would have been noticed as a dead mic.
            let mut last = Vec::new();
            for frame_idx in 0..500 {
                let frame: Vec<f32> = (0..FRAME)
                    .map(|i| {
                        let n = (frame_idx * FRAME + i) as f64;
                        (std::f64::consts::TAU * 1000.0 * n / RATE).sin() as f32
                    })
                    .collect();
                last = filter.process(&frame).to_vec();
            }
            assert!(
                last.iter().all(|s| s.is_finite()),
                "{poison} latched the recursion"
            );
        }
    }

    #[test]
    fn reset_clears_the_recursion() {
        let mut filter = HighPass::new(CUTOFF, RATE, FRAME);
        let mut impulse = vec![0.0f32; FRAME];
        impulse[0] = 32768.0;
        filter.process(&impulse);
        filter.reset();
        let zeros = vec![0.0f32; FRAME];
        assert!(filter.process(&zeros).iter().all(|&s| s == 0.0));
    }
}
