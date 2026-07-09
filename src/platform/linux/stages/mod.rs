//! Per-frame DSP stages composed by `AudioProcessor`. Each stage is a plain
//! struct with preallocated state — safe on the real-time audio thread.

pub mod gate;
pub mod highpass;
pub mod leveler;

pub use gate::GateEnvelope;
pub use highpass::HighPass;
pub use leveler::Leveler;
