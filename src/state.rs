use serde::{Deserialize, Serialize};
use std::sync::atomic::AtomicI32;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

/// The process-wide locking policy: recover from poisoned mutexes instead of
/// panicking. The long-lived daemon prefers potentially stale data over a
/// crash (or an abort when the panic would cross the PipeWire FFI boundary),
/// and every guarded value here is a single whole value with no cross-field
/// invariants, so recovery can never observe a torn write. The original panic
/// that poisoned the lock is still reported by the panic hook.
pub fn lock_or_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AudioDevice {
    pub id: u32,
    pub name: String,
    /// Number of audio channels (1=mono, 2=stereo, etc.)
    /// TODO: Validate channel handling for devices with >2 channels (e.g., multichannel interfaces)
    #[serde(default = "default_channels")]
    pub channels: u32,
}

fn default_channels() -> u32 {
    2 // Default to stereo for backwards compatibility
}

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub enum Status {
    Inactive,
    Active,
    Pending,
}

pub struct AppState {
    pub status: Arc<Mutex<Status>>,
    pub selected_device_id: Arc<Mutex<Option<u32>>>,
    /// Atomic so the real-time audio path can read it without locking.
    pub threshold_val: Arc<AtomicI32>,
    pub mic_name: Arc<Mutex<String>>,
    /// Why the mic is not active: set on pipeline death or a failed
    /// enable/disable, cleared when an enable or disable succeeds.
    pub last_error: Arc<Mutex<Option<String>>>,
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

impl AppState {
    pub fn new() -> Self {
        Self {
            status: Arc::new(Mutex::new(Status::Inactive)),
            selected_device_id: Arc::new(Mutex::new(None)),
            threshold_val: Arc::new(AtomicI32::new(80)),
            mic_name: Arc::new(Mutex::new("Spectral-Mic".to_string())),
            last_error: Arc::new(Mutex::new(None)),
        }
    }

    pub fn update_status(&self, new_status: Status) {
        *lock_or_recover(&self.status) = new_status;
    }

    pub fn set_last_error(&self, error: Option<String>) {
        *lock_or_recover(&self.last_error) = error;
    }
}

pub const MIC_NAME_MAX_LEN: usize = 21;

pub fn is_valid_mic_name(name: &str) -> bool {
    let len = name.chars().count();
    len > 0
        && len <= MIC_NAME_MAX_LEN
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// PulseAudio node names derived from a mic name. `VirtualMic::create` owns this
/// scheme; every other consumer (e.g. filtered recording) derives names through
/// these helpers so the sink and its monitor can never drift out of sync.
pub fn sink_name(mic_name: &str) -> String {
    format!("{mic_name}_sink")
}

pub fn monitor_name(mic_name: &str) -> String {
    format!("{}.monitor", sink_name(mic_name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_the_pulseaudio_scheme() {
        assert_eq!(sink_name("Spectral-Mic"), "Spectral-Mic_sink");
        assert_eq!(monitor_name("Spectral-Mic"), "Spectral-Mic_sink.monitor");
        assert_eq!(monitor_name("x"), format!("{}.monitor", sink_name("x")));
    }
}
