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

/// Returns a safe base config directory, validating against path traversal.
/// Uses XDG_CONFIG_HOME if set and valid, otherwise falls back to ~/.config.
fn get_safe_config_base() -> Result<PathBuf, io::Error> {
    // Try XDG_CONFIG_HOME first
    if let Ok(xdg_config) = std::env::var("XDG_CONFIG_HOME") {
        let path = PathBuf::from(&xdg_config);
        // Validate: must be absolute and not contain path traversal
        if path.is_absolute() && !xdg_config.contains("..") {
            return Ok(path);
        }
        // Invalid XDG_CONFIG_HOME, fall through to HOME-based default
        eprintln!(
            "Warning: XDG_CONFIG_HOME contains invalid path '{}', using fallback",
            xdg_config
        );
    }

    // Fall back to HOME/.config
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE")) // Windows compatibility
        .map_err(|_| io::Error::new(ErrorKind::NotFound, "Neither HOME nor USERPROFILE is set"))?;

    let home_path = PathBuf::from(&home);
    if !home_path.is_absolute() || home.contains("..") {
        return Err(io::Error::new(
            ErrorKind::InvalidInput,
            format!("HOME contains invalid path: {}", home),
        ));
    }

    Ok(home_path.join(".config"))
}

pub fn get_config_path() -> PathBuf {
    get_safe_config_base()
        .unwrap_or_else(|e| {
            eprintln!(
                "Warning: Could not determine config base: {}, using /tmp",
                e
            );
            PathBuf::from("/tmp")
        })
        .join("spectral")
        .join("state.toml")
}

pub fn get_recordings_dir() -> PathBuf {
    let recordings_dir = get_safe_config_base()
        .unwrap_or_else(|e| {
            eprintln!(
                "Warning: Could not determine config base: {}, using /tmp",
                e
            );
            PathBuf::from("/tmp")
        })
        .join("spectral")
        .join("recordings");

    // Ensure the directory exists
    if let Err(e) = fs::create_dir_all(&recordings_dir) {
        eprintln!("Failed to create recordings directory: {}", e);
    }

    recordings_dir
}

pub fn get_original_recording_path() -> PathBuf {
    get_recordings_dir().join("original_output.wav")
}

pub fn get_filtered_recording_path() -> PathBuf {
    get_recordings_dir().join("filtered_output.wav")
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
