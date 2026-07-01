use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, ErrorKind};
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PersistedState {
    pub device_id: Option<u32>,
    pub mic_name: String,
    pub threshold: i32,
}

impl Default for PersistedState {
    fn default() -> Self {
        Self {
            device_id: None,
            mic_name: "Spectral-Mic".to_string(),
            threshold: 80,
        }
    }
}

fn get_safe_config_base() -> Result<PathBuf, io::Error> {
    if let Ok(xdg_config) = std::env::var("XDG_CONFIG_HOME") {
        let path = PathBuf::from(&xdg_config);
        if path.is_absolute() && !xdg_config.contains("..") {
            return Ok(path);
        }
        eprintln!(
            "Warning: XDG_CONFIG_HOME contains invalid path '{}', using fallback",
            xdg_config
        );
    }

    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_state_format_with_is_active_still_loads() {
        let old_format =
            "is_active = true\ndevice_id = 42\nmic_name = \"My-Mic\"\nthreshold = 65\n";
        let state: PersistedState = toml::from_str(old_format).expect("old format must load");
        assert_eq!(state.device_id, Some(42));
        assert_eq!(state.mic_name, "My-Mic");
        assert_eq!(state.threshold, 65);
    }
}
