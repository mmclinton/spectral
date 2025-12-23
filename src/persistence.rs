use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, ErrorKind};
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PersistedState {
    pub is_active: bool,
    pub device_id: Option<u32>,
    pub mic_name: String,
    pub threshold: i32,
}

impl Default for PersistedState {
    fn default() -> Self {
        Self {
            is_active: false,
            device_id: None,
            mic_name: "Spectral-Mic".to_string(),
            threshold: 80,
        }
    }
}

pub fn get_config_path() -> PathBuf {
    let config_dir = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").expect("HOME not set");
            PathBuf::from(home).join(".config")
        })
        .join("spectral");

    config_dir.join("state.toml")
}

pub fn load_state() -> Result<PersistedState, io::Error> {
    let path = get_config_path();

    if !path.exists() {
        return Ok(PersistedState::default());
    }

    let contents = fs::read_to_string(&path)?;
    toml::from_str(&contents).map_err(|e| io::Error::new(ErrorKind::InvalidData, e))
}

pub fn save_state(state: &PersistedState) -> Result<(), io::Error> {
    let path = get_config_path();

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let toml_string =
        toml::to_string(state).map_err(|e| io::Error::new(ErrorKind::InvalidData, e))?;

    fs::write(&path, toml_string)
}
