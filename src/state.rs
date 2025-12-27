use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AudioDevice {
    pub id: u32,
    pub name: String,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub enum Status {
    Inactive,
    Active,
    Pending,
}

pub struct AppState {
    pub status: Arc<Mutex<Status>>,
    pub selected_input: Arc<Mutex<Option<usize>>>,
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
            selected_input: Arc::new(Mutex::new(None)),
            selected_device_id: Arc::new(Mutex::new(None)),
            threshold_val: Arc::new(Mutex::new(80)),
            mic_name: Arc::new(Mutex::new("Spectral-Mic".to_string())),
            pending_threshold: Arc::new(Mutex::new(None)),
            pending_device_id: Arc::new(Mutex::new(None)),
            pending_mic_name: Arc::new(Mutex::new(None)),
        }
    }

    pub fn update_status(&self, new_status: Status) {
        *self.status.lock().unwrap() = new_status;
    }
}
