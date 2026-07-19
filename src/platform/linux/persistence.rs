//! XDG-compliant TOML persistence for daemon settings, plus the on-disk
//! locations of the voice-test recordings.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};

/// Settings that survive daemon restarts, stored as TOML under the XDG
/// config dir. New fields must be `#[serde(default)]` so files written by
/// older versions still load.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PersistedState {
    /// PipeWire ids are scoped to one server instance; the node name below is
    /// the durable handle across reboots and PipeWire restarts.
    pub device_id: Option<u32>,
    /// The selected device's PipeWire `node.name` — the durable handle.
    #[serde(default)]
    pub device_node_name: Option<String>,
    /// Display name for the virtual mic node.
    pub mic_name: String,
    /// Voice-gate threshold as shown in the UI.
    pub threshold: i32,
    /// Whether the high-pass rumble filter is enabled.
    #[serde(default)]
    pub rumble_filter: bool,
    /// Whether the auto-level stage is enabled.
    #[serde(default)]
    pub auto_level: bool,
}

impl Default for PersistedState {
    fn default() -> Self {
        Self {
            device_id: None,
            device_node_name: None,
            mic_name: "Spectral-Mic".to_string(),
            threshold: 50,
            rumble_filter: false,
            auto_level: false,
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

/// Path of the settings file, `$XDG_CONFIG_HOME/spectral/state.toml` (or the
/// `~/.config` fallback); degrades to `/tmp` when no valid base exists so
/// the daemon keeps running with ephemeral settings rather than dying.
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

/// Directory holding the voice-test recordings, under the XDG state dir
/// (recordings are regenerable audio, not configuration); created on demand.
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

/// Location of the raw (pre-processing) voice-test capture.
pub fn get_original_recording_path() -> PathBuf {
    get_recordings_dir().join("original_output.wav")
}

/// Location of the processed voice-test capture.
pub fn get_filtered_recording_path() -> PathBuf {
    get_recordings_dir().join("filtered_output.wav")
}

/// Loads persisted settings, returning defaults when no file exists yet.
pub fn load_state() -> Result<PersistedState, io::Error> {
    let path = get_config_path();

    if !path.exists() {
        return Ok(PersistedState::default());
    }

    let contents = fs::read_to_string(&path)?;
    toml::from_str(&contents).map_err(|e| io::Error::new(ErrorKind::InvalidData, e))
}

/// Writes settings as TOML, creating the config directory if needed.
pub fn save_state(state: &PersistedState) -> Result<(), io::Error> {
    let path = get_config_path();

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    write_state_to(&path, state)
}

/// Serializes `state` to `path` through a same-directory temp file and a
/// rename, so a crash or power loss mid-write can never leave a torn
/// half-file behind (rename is atomic within one filesystem on POSIX).
fn write_state_to(path: &Path, state: &PersistedState) -> Result<(), io::Error> {
    let toml_string =
        toml::to_string(state).map_err(|e| io::Error::new(ErrorKind::InvalidData, e))?;

    let tmp = path.with_extension("toml.tmp");
    fs::write(&tmp, toml_string)?;
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_and_stale_state_formats_still_load() {
        // `is_active` predates 0.2.x; `voice_smoothing` existed only in
        // unreleased builds. Unknown keys must be ignored, missing toggles
        // default off.
        let old_format = "is_active = true\ndevice_id = 42\nmic_name = \"My-Mic\"\nthreshold = 65\nvoice_smoothing = true\n";
        let state: PersistedState = toml::from_str(old_format).expect("old format must load");
        assert_eq!(state.device_id, Some(42));
        assert_eq!(state.device_node_name, None);
        assert_eq!(state.mic_name, "My-Mic");
        assert_eq!(state.threshold, 65);
        assert!(!state.rumble_filter);
        assert!(!state.auto_level);
    }

    #[test]
    fn save_replaces_the_file_atomically_and_cleans_up_its_temp() {
        let dir = std::env::temp_dir().join("spectral_persistence_atomic_save");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.toml");

        write_state_to(&path, &PersistedState::default()).unwrap();
        let loaded: PersistedState = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(loaded.mic_name, "Spectral-Mic");
        assert!(
            !path.with_extension("toml.tmp").exists(),
            "temp file must be renamed away"
        );

        // Rename over an existing file replaces it in one step.
        let updated = PersistedState {
            threshold: 75,
            ..PersistedState::default()
        };
        write_state_to(&path, &updated).unwrap();
        let loaded: PersistedState = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(loaded.threshold, 75);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn device_node_name_roundtrips_present_and_absent() {
        for node_name in [Some("alsa_input.usb-Blue_Yeti-00.analog-stereo"), None] {
            let state = PersistedState {
                device_id: Some(57),
                device_node_name: node_name.map(str::to_string),
                mic_name: "My-Mic".to_string(),
                threshold: 65,
                rumble_filter: true,
                auto_level: true,
            };
            let toml_string = toml::to_string(&state).expect("state must serialize");
            let loaded: PersistedState =
                toml::from_str(&toml_string).expect("state must roundtrip");
            assert_eq!(loaded.device_id, state.device_id);
            assert_eq!(loaded.device_node_name, state.device_node_name);
            assert_eq!(loaded.mic_name, state.mic_name);
            assert_eq!(loaded.threshold, state.threshold);
            assert_eq!(loaded.rumble_filter, state.rumble_filter);
            assert_eq!(loaded.auto_level, state.auto_level);
        }
    }
}
