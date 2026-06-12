use serde::{Deserialize, Serialize};
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
    pub threshold_val: Arc<Mutex<i32>>,
    pub mic_name: Arc<Mutex<String>>,
    pub is_recording: Arc<Mutex<bool>>,
    pub has_recording: Arc<Mutex<bool>>,
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
            threshold_val: Arc::new(Mutex::new(80)),
            mic_name: Arc::new(Mutex::new("Spectral-Mic".to_string())),
            is_recording: Arc::new(Mutex::new(false)),
            has_recording: Arc::new(Mutex::new(false)),
        }
    }

    pub fn update_status(&self, new_status: Status) {
        *lock_or_recover(&self.status) = new_status;
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

pub fn sanitize_mic_name(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .take(MIC_NAME_MAX_LEN)
        .collect()
}
