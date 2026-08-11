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
    /// Channel count as reported by the node's own info props (1 = mono);
    /// 2 can also mean "unknown" — the fallback when a node dies before
    /// reporting or omits `audio.channels`.
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

/// Upper bound for user-chosen virtual-mic names. Compared against byte
/// length, which equals character count for every name the rule accepts.
/// Sized so a max-length name fits the GUI name field without scrolling.
pub const MIC_NAME_MAX_LEN: usize = 20;

/// Validates a user-chosen mic name: non-empty, at most
/// [`MIC_NAME_MAX_LEN`] characters, ASCII alphanumerics plus `_` and `-` —
/// the name is embedded in PipeWire node properties, so it stays
/// conservative.
///
/// Byte-wise on purpose: every permitted character is ASCII, so any name
/// that passes the character test has byte length equal to char count, and
/// an oversized paste is rejected without decoding it end-to-end.
pub fn is_valid_mic_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MIC_NAME_MAX_LEN
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// The rule [`is_valid_mic_name`] enforces, in the words shown to the user.
///
/// Only the length is derived. The character class is prose, so relaxing
/// the byte test above means editing this string in the same change — it
/// sits here, one screen away, so that the edit is hard to miss rather
/// than scattered across the CLI, the daemon and the pipeline.
pub fn mic_name_rule() -> String {
    format!("only [A-Za-z0-9_-] allowed, 1-{MIC_NAME_MAX_LEN} chars")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mic_names_are_ascii_alphanumerics_underscore_and_hyphen() {
        let cases = [
            ("Spectral-Mic", true),
            ("a", true),
            ("A1_b-2", true),
            ("_", true),
            ("-", true),
            ("", false),
            ("has space", false),
            ("bang!", false),
            ("dot.separated", false),
            ("slash/es", false),
            ("caf\u{e9}", false),
            ("\u{1f3a4}", false),
            ("tab\there", false),
            ("new\nline", false),
        ];
        for (name, expected) in cases {
            assert_eq!(is_valid_mic_name(name), expected, "name {name:?}");
        }
    }

    #[test]
    fn mic_name_length_bound_admits_the_maximum_and_rejects_one_past_it() {
        assert!(is_valid_mic_name(&"x".repeat(MIC_NAME_MAX_LEN)));
        assert!(!is_valid_mic_name(&"x".repeat(MIC_NAME_MAX_LEN + 1)));
    }

    /// Byte and char counting cannot diverge while the character class is
    /// ASCII-only: multi-byte input is rejected on its characters whichever
    /// length the gate compared. This pins that equivalence where it is
    /// least obvious — a name that exactly fills the byte budget, and so
    /// clears the length gate on either reading. The fixture is asserted
    /// onto that boundary rather than assumed onto it, so a changed
    /// [`MIC_NAME_MAX_LEN`] fails loudly instead of silently sliding off it.
    #[test]
    fn multi_byte_input_at_the_byte_bound_is_still_rejected() {
        let multi_byte = "\u{e9}".repeat(MIC_NAME_MAX_LEN / 2);
        let pad = "x".repeat(MIC_NAME_MAX_LEN - multi_byte.len());
        let name = format!("{multi_byte}{pad}");

        assert_eq!(name.len(), MIC_NAME_MAX_LEN);
        assert!(name.chars().count() < MIC_NAME_MAX_LEN);
        assert!(!is_valid_mic_name(&name));
    }
}
