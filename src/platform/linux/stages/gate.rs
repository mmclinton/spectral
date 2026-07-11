//! Voice gate: turns RNNoise's per-frame VAD confidence into a click-free
//! gain using lookahead, hysteresis, hold, and an attenuation floor.
//!
//! The gate delays audio by [`LOOKAHEAD_FRAMES`] while VAD decisions run on
//! the undelayed stream, so the gain is already up when a speech onset
//! plays — RNNoise's VAD lags onsets by a frame or two, which no attack
//! speed alone can compensate. The delay is constant (also in denoise-only
//! bypass), so pipeline latency never shifts with settings.

/// Frames of audio delay giving the gate its head start (2 × 10 ms).
pub const LOOKAHEAD_FRAMES: usize = 2;

// Frames the gain holds at exactly 1.0 after the last voiced frame (200 ms
// — the reference RNNoise plugin's grace-period default), so natural
// intra-sentence pauses never start a release.
const HOLD_FRAMES: u32 = 20;

// Linear gain added per frame while opening: floor to unity inside the
// lookahead window.
const ATTACK_STEP: f32 = 0.5;

// Multiplicative decay per frame after the hold expires: reaches the floor
// in ~80 ms.
const RELEASE: f32 = 0.75;

// The closed gate attenuates to -20 dB instead of muting: the residual is
// already denoised, and quiet room tone reads as natural where digital
// silence reads as processing.
const FLOOR: f32 = 0.1;

// A frame classified voice stays voice until the VAD falls below this
// fraction of the open cutoff, so confidence flutter around the threshold
// cannot chatter the gate.
const CLOSE_RATIO: f32 = 2.0 / 3.0;

/// Per-instance gate state: the lookahead delay line plus the open/close
/// decision machinery described in the module docs.
pub struct VoiceGate {
    delay: [Vec<f32>; LOOKAHEAD_FRAMES],
    queued_voice: [bool; LOOKAHEAD_FRAMES],
    head: usize,
    voice_state: bool,
    hold_remaining: u32,
    gain: f32,
}

impl VoiceGate {
    /// `frame_len` sizes the delay slots so the audio path never allocates.
    pub fn new(frame_len: usize) -> Self {
        Self {
            delay: [vec![0.0; frame_len], vec![0.0; frame_len]],
            queued_voice: [false; LOOKAHEAD_FRAMES],
            head: 0,
            voice_state: false,
            hold_remaining: 0,
            gain: FLOOR,
        }
    }

    /// Feed one frame and its VAD confidence; `frame` is replaced with the
    /// audio from [`LOOKAHEAD_FRAMES`] calls ago, gated. `open_cutoff` of
    /// `None` bypasses the gate (gain ramps to unity; the delay remains).
    /// The first [`LOOKAHEAD_FRAMES`] calls emit the priming silence.
    pub fn process(&mut self, frame: &mut [f32], vad: f32, open_cutoff: Option<f32>) {
        debug_assert_eq!(frame.len(), self.delay[0].len());

        let voice_now = match open_cutoff {
            Some(open) => {
                let cutoff = if self.voice_state {
                    open * CLOSE_RATIO
                } else {
                    open
                };
                self.voice_state = vad > cutoff;
                self.voice_state
            }
            None => {
                self.voice_state = false;
                self.hold_remaining = 0;
                false
            }
        };

        // Swap the incoming frame with the oldest queued one; decisions
        // travel with their frames.
        let emitted_voice = self.queued_voice[self.head];
        let middle_voice = self.queued_voice[1 - self.head];
        self.queued_voice[self.head] = voice_now;
        for (sample, queued) in frame.iter_mut().zip(self.delay[self.head].iter_mut()) {
            std::mem::swap(sample, queued);
        }
        self.head = 1 - self.head;

        // The emitted frame refreshes the hold; the hold covers the frames
        // after it. Lookahead: the two younger decisions pre-open the gate.
        let held = if emitted_voice {
            self.hold_remaining = HOLD_FRAMES;
            true
        } else if self.hold_remaining > 0 {
            self.hold_remaining -= 1;
            true
        } else {
            false
        };
        let open_target = open_cutoff.is_none() || held || middle_voice || voice_now;

        let start = self.gain;
        let target = if open_target {
            (start + ATTACK_STEP).min(1.0)
        } else {
            (start * RELEASE).max(FLOOR)
        };
        let step = (target - start) / frame.len() as f32;
        for (i, sample) in frame.iter_mut().enumerate() {
            *sample *= start + step * (i + 1) as f32;
        }
        self.gain = target;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: usize = 8;
    const OPEN: f32 = 0.76;

    fn run(gate: &mut VoiceGate, value: f32, vad: f32, cutoff: Option<f32>) -> Vec<f32> {
        let mut frame = vec![value; FRAME];
        gate.process(&mut frame, vad, cutoff);
        frame
    }

    #[test]
    fn audio_is_delayed_exactly_two_frames_and_bypass_is_unity() {
        let mut gate = VoiceGate::new(FRAME);
        assert!(run(&mut gate, 5.0, 0.0, None).iter().all(|&s| s == 0.0));
        assert!(run(&mut gate, 6.0, 0.0, None).iter().all(|&s| s == 0.0));
        // Two priming frames were enough for the gain to reach unity, so
        // the first real frame emerges bit-identical.
        assert!(run(&mut gate, 7.0, 0.0, None).iter().all(|&s| s == 5.0));
        assert!(run(&mut gate, 8.0, 0.0, None).iter().all(|&s| s == 6.0));
    }

    #[test]
    fn onset_is_pre_opened_by_the_lookahead() {
        let mut gate = VoiceGate::new(FRAME);
        for _ in 0..30 {
            run(&mut gate, 1.0, 0.0, Some(OPEN));
        }
        // Voice starts now; its audio emerges two calls later and must play
        // at full gain because the lookahead opened the gate early.
        run(&mut gate, 2.0, 0.9, Some(OPEN));
        run(&mut gate, 2.0, 0.9, Some(OPEN));
        let first_voice_out = run(&mut gate, 2.0, 0.9, Some(OPEN));
        assert!(
            first_voice_out.iter().all(|&s| s >= 2.0 * 0.99),
            "onset not at unity: {first_voice_out:?}"
        );
    }

    #[test]
    fn hold_keeps_unity_then_release_lands_on_the_floor() {
        let mut gate = VoiceGate::new(FRAME);
        for _ in 0..10 {
            run(&mut gate, 1.0, 0.9, Some(OPEN));
        }
        // Voice ends. The last voiced frame emerges after the 2 queued
        // calls; from there the hold covers HOLD_FRAMES emitted frames.
        let mut unity_frames = 0;
        let mut last = Vec::new();
        for _ in 0..(HOLD_FRAMES + 40) {
            last = run(&mut gate, 1.0, 0.0, Some(OPEN));
            if last.iter().all(|&s| s == 1.0) {
                unity_frames += 1;
            }
        }
        // 2 queued voice frames + HOLD_FRAMES of hold.
        assert_eq!(unity_frames, 2 + HOLD_FRAMES as i32);
        assert!(last.iter().all(|&s| (s - FLOOR).abs() < 1e-6), "{last:?}");
        // And it stays on the floor, never below.
        let settled = run(&mut gate, 1.0, 0.0, Some(OPEN));
        assert!(settled.iter().all(|&s| (s - FLOOR).abs() < 1e-6));
    }

    #[test]
    fn hysteresis_keeps_the_gate_open_through_confidence_flutter() {
        let mut gate = VoiceGate::new(FRAME);
        for _ in 0..5 {
            run(&mut gate, 1.0, 0.9, Some(OPEN));
        }
        // Flutter between the close cutoff (0.507) and the open cutoff:
        // still classified voice, so the gain never leaves unity and the
        // hold keeps refreshing.
        for _ in 0..50 {
            let out = run(&mut gate, 1.0, 0.6, Some(OPEN));
            assert!(out.iter().all(|&s| s == 1.0), "fluttered: {out:?}");
        }
        // Below the close cutoff the gate finally closes (hold, then
        // release to the floor).
        let mut last = Vec::new();
        for _ in 0..(HOLD_FRAMES + 40) {
            last = run(&mut gate, 1.0, 0.4, Some(OPEN));
        }
        assert!(last.iter().all(|&s| (s - FLOOR).abs() < 1e-6));
        // A fresh gate would NOT have opened at 0.6 in the first place.
        let mut closed = VoiceGate::new(FRAME);
        for _ in 0..30 {
            let out = run(&mut closed, 1.0, 0.6, Some(OPEN));
            assert!(out.iter().all(|&s| s <= FLOOR + 1e-6), "opened: {out:?}");
        }
    }

    #[test]
    fn gain_ramps_within_a_frame_instead_of_stepping() {
        let mut gate = VoiceGate::new(FRAME);
        for _ in 0..30 {
            run(&mut gate, 1.0, 0.0, Some(OPEN));
        }
        // First opening frame: gain moves FLOOR -> FLOOR+ATTACK_STEP,
        // interpolated per sample.
        let out = run(&mut gate, 1.0, 0.9, Some(OPEN));
        for (i, sample) in out.iter().enumerate() {
            let expected = FLOOR + ATTACK_STEP * (i + 1) as f32 / FRAME as f32;
            assert!(
                (sample - expected).abs() < 1e-6,
                "sample {i}: got {sample}, expected {expected}"
            );
        }
    }
}
