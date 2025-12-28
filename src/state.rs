use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

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
    // Track pending (UI) vs applied (active pipeline) values
    pub pending_threshold: Arc<Mutex<Option<i32>>>,
    pub pending_device_id: Arc<Mutex<Option<u32>>>,
    pub pending_mic_name: Arc<Mutex<Option<String>>>,
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
            pending_threshold: Arc::new(Mutex::new(None)),
            pending_device_id: Arc::new(Mutex::new(None)),
            pending_mic_name: Arc::new(Mutex::new(None)),
        }
    }

    pub fn update_status(&self, new_status: Status) {
        *self
            .status
            .lock()
            .expect("status mutex poisoned in update_status") = new_status;
    }
}
