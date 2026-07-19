//! Shared daemon state and the vocabulary types both binaries exchange:
//! device descriptions, the mic lifecycle status, and mic-name validation.

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, AtomicI32};
use std::sync::{Mutex, MutexGuard, PoisonError};

/// The process-wide locking policy: recover from poisoned mutexes instead of
/// panicking. The long-lived daemon prefers potentially stale data over a
/// crash (or an abort when the panic would cross the PipeWire FFI boundary),
/// and every guarded value here is a single whole value with no cross-field
/// invariants, so recovery can never observe a torn write. The original panic
/// that poisoned the lock is still reported by the panic hook.
pub fn lock_or_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// An audio input source as enumerated from the PipeWire registry.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AudioDevice {
    /// PipeWire node id — valid only for the current server instance.
    pub id: u32,
    /// Human-readable description shown in device pickers.
    pub name: String,
    /// Number of audio channels (1=mono, 2=stereo, etc.)
    /// TODO: Validate channel handling for devices with >2 channels (e.g., multichannel interfaces)
    #[serde(default = "default_channels")]
    pub channels: u32,
}

fn default_channels() -> u32 {
    2 // Default to stereo for backwards compatibility
}

/// The virtual mic's lifecycle state.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub enum Status {
    /// No virtual mic exists.
    Inactive,
    /// The pipeline is running and the virtual mic is available.
    Active,
    /// Mid-transition: an enable or disable has started but not settled.
    Pending,
}

/// The daemon's shared state, shared as a whole behind one
/// `Arc<AppState>`. Each field carries its own lock (or is atomic, for the
/// values the real-time audio path reads) so threads contend per value,
/// never on one big lock.
pub struct AppState {
    /// Current lifecycle state of the virtual mic.
    pub status: Mutex<Status>,
    /// PipeWire id of the chosen capture device, if one is selected.
    pub selected_device_id: Mutex<Option<u32>>,
    /// The selected device's PipeWire node.name — the durable handle; ids are
    /// scoped to one server instance.
    pub device_node_name: Mutex<Option<String>>,
    /// Atomic so the real-time audio path can read it without locking.
    pub threshold_val: AtomicI32,
    /// High-pass rumble cut ahead of the denoiser. Atomic for the same
    /// reason as `threshold_val`.
    pub rumble_filter: AtomicBool,
    /// VAD-frozen automatic gain riding with a peak limiter. Atomic for the
    /// same reason as `threshold_val`.
    pub auto_level: AtomicBool,
    /// Display name for the virtual mic node.
    pub mic_name: Mutex<String>,
    /// Why the mic is not active: set on pipeline death or a failed
    /// enable/disable, cleared when an enable or disable succeeds.
    pub last_error: Mutex<Option<String>>,
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

impl AppState {
    /// Creates state with the built-in defaults: inactive, threshold 50,
    /// both voice stages off, mic named "Spectral-Mic".
    pub fn new() -> Self {
        Self {
            status: Mutex::new(Status::Inactive),
            selected_device_id: Mutex::new(None),
            device_node_name: Mutex::new(None),
            threshold_val: AtomicI32::new(50),
            rumble_filter: AtomicBool::new(false),
            auto_level: AtomicBool::new(false),
            mic_name: Mutex::new("Spectral-Mic".to_string()),
            last_error: Mutex::new(None),
        }
    }

    /// Replaces the mic's lifecycle status.
    pub fn update_status(&self, new_status: Status) {
        *lock_or_recover(&self.status) = new_status;
    }

    /// Records why the mic is down, or clears the record with `None`.
    pub fn set_last_error(&self, error: Option<String>) {
        *lock_or_recover(&self.last_error) = error;
    }
}

/// Upper bound for user-chosen virtual-mic names, in characters. Sized so
/// a max-length name fits the GUI name field without scrolling.
pub const MIC_NAME_MAX_LEN: usize = 20;

/// Validates a user-chosen mic name: non-empty, at most
/// [`MIC_NAME_MAX_LEN`] characters, ASCII alphanumerics plus `_` and `-` —
/// the name is embedded in PipeWire node properties, so it stays
/// conservative.
pub fn is_valid_mic_name(name: &str) -> bool {
    let len = name.chars().count();
    len > 0
        && len <= MIC_NAME_MAX_LEN
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}
