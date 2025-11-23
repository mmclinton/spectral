use std::sync::{Arc, Mutex};

#[derive(Clone, Debug)]
pub struct AudioDevice {
    pub id: u32,
    pub name: String,
}

#[derive(Clone, PartialEq)]
pub enum Status {
    Inactive,
    Active,
    Pending,
}

pub(crate) struct AppState {
    pub status: Arc<Mutex<Status>>,
    pub selected_input: Arc<Mutex<Option<usize>>>,
    pub selected_device_id: Arc<Mutex<Option<u32>>>,
    pub threshold_val: Arc<Mutex<i32>>,
    pub mic_name: Arc<Mutex<String>>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            status: Arc::new(Mutex::new(Status::Inactive)),
            selected_input: Arc::new(Mutex::new(None)),
            selected_device_id: Arc::new(Mutex::new(None)),
            threshold_val: Arc::new(Mutex::new(80)),
            mic_name: Arc::new(Mutex::new("Spectral-Mic".to_string())),
        }
    }

    pub fn update_status(&self, new_status: Status) {
        *self.status.lock().unwrap() = new_status;
    }
}
