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

fn safe_base_dir(xdg_var: &str, home_suffix: &[&str]) -> Result<PathBuf, io::Error> {
    if let Ok(xdg_value) = std::env::var(xdg_var) {
        let path = PathBuf::from(&xdg_value);
        if path.is_absolute() && !xdg_value.contains("..") {
            return Ok(path);
        }
        eprintln!("Warning: {xdg_var} contains invalid path '{xdg_value}', using fallback");
    }

    let home = std::env::var("HOME")
        .map_err(|_| io::Error::new(ErrorKind::NotFound, "HOME is not set"))?;

    let home_path = PathBuf::from(&home);
    if !home_path.is_absolute() || home.contains("..") {
        return Err(io::Error::new(
            ErrorKind::InvalidInput,
            format!("HOME contains invalid path: {}", home),
        ));
    }

    Ok(home_suffix
        .iter()
        .fold(home_path, |path, part| path.join(part)))
}

fn get_safe_config_base() -> Result<PathBuf, io::Error> {
    safe_base_dir("XDG_CONFIG_HOME", &[".config"])
}

fn get_safe_state_base() -> Result<PathBuf, io::Error> {
    safe_base_dir("XDG_STATE_HOME", &[".local", "state"])
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
    let recordings_dir = get_safe_state_base()
        .unwrap_or_else(|e| {
            eprintln!("Warning: Could not determine state base: {}, using /tmp", e);
            PathBuf::from("/tmp")
        })
        .join("spectral")
        .join("recordings");

    if let Err(e) = fs::create_dir_all(&recordings_dir) {
        eprintln!("Failed to create recordings directory: {}", e);
    }

    recordings_dir
}

/// Recordings lived under the config dir through v0.2.2; sweep the two known
/// files best-effort so config-dir backups stop syncing stale audio. Anything
/// else a user placed there is left alone.
pub fn remove_legacy_recordings() {
    let Ok(base) = get_safe_config_base() else {
        return;
    };
    let legacy = base.join("spectral").join("recordings");
    for name in ["original_output.wav", "filtered_output.wav"] {
        let path = legacy.join(name);
        if let Err(e) = fs::remove_file(&path)
            && e.kind() != ErrorKind::NotFound
        {
            eprintln!(
                "Failed to remove legacy recording {}: {}",
                path.display(),
                e
            );
        }
    }
    if let Err(e) = fs::remove_dir(&legacy)
        && !matches!(e.kind(), ErrorKind::NotFound | ErrorKind::DirectoryNotEmpty)
    {
        eprintln!("Failed to remove legacy recordings dir: {}", e);
    }
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
