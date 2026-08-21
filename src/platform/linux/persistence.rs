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
/// `~/.config` fallback).
///
/// There is deliberately no second location. The obvious fallback is a fixed
/// path under `/tmp`, and that is a symlink target on a shared system: every
/// write here either truncates (`state.toml.tmp`) or is read back and trusted
/// (`state.toml`), and neither `fs::write` nor `fs::read_to_string` refuses to
/// follow a link another user planted first. `daemon_launch::daemon_log_path`
/// already refuses `/tmp` for the same reason, and the payload here — the
/// settings the daemon acts on, and next to them the voice-test captures — is
/// worth more than the log. A daemon that cannot name a private directory
/// should decline to persist, not persist somewhere world-writable.
pub fn get_config_path() -> Result<PathBuf, io::Error> {
    Ok(get_safe_config_base()?.join("spectral").join("state.toml"))
}

/// Directory holding the voice-test recordings, under the XDG state dir
/// (recordings are regenerable audio, not configuration); created on demand.
///
/// Refuses to fall back to `/tmp` for the reason given on
/// [`get_config_path`], which applies with more force here: these are raw
/// microphone captures.
pub fn get_recordings_dir() -> Result<PathBuf, io::Error> {
    let recordings_dir = get_safe_state_base()?.join("spectral").join("recordings");
    fs::create_dir_all(&recordings_dir)?;
    Ok(recordings_dir)
}

/// Location of the raw (pre-processing) voice-test capture.
pub fn get_original_recording_path() -> Result<PathBuf, io::Error> {
    Ok(get_recordings_dir()?.join("original_output.wav"))
}

/// Location of the processed voice-test capture.
pub fn get_filtered_recording_path() -> Result<PathBuf, io::Error> {
    Ok(get_recordings_dir()?.join("filtered_output.wav"))
}

/// Loads persisted settings, returning defaults when no file exists yet.
pub fn load_state() -> Result<PersistedState, io::Error> {
    let path = get_config_path()?;

    if !path.exists() {
        return Ok(PersistedState::default());
    }

    let contents = fs::read_to_string(&path)?;
    toml::from_str(&contents).map_err(|e| io::Error::new(ErrorKind::InvalidData, e))
}

/// Writes settings as TOML, creating the config directory if needed.
pub fn save_state(state: &PersistedState) -> Result<(), io::Error> {
    let path = get_config_path()?;

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

    /// The half of the schema contract that lets a new version read an old
    /// file: a file written before these keys existed still loads, with each
    /// of them at its default. What that costs an author differs by type —
    /// `#[serde(default)]` is what carries `rumble_filter` and `auto_level`,
    /// while `Option` fields get `None` from serde whether or not the
    /// attribute is there. The assertions below are on the behavior, so they
    /// hold either way; only the non-`Option` half is enforced by removing
    /// the attribute.
    #[test]
    fn absent_optional_keys_fall_back_to_their_defaults() {
        let without_the_optional_keys = "device_id = 42\nmic_name = \"My-Mic\"\nthreshold = 65\n";
        let state: PersistedState =
            toml::from_str(without_the_optional_keys).expect("a file missing them must load");
        assert_eq!(state.device_id, Some(42));
        assert_eq!(state.mic_name, "My-Mic");
        assert_eq!(state.threshold, 65);
        assert_eq!(state.device_node_name, None);
        assert!(!state.rumble_filter);
        assert!(!state.auto_level);
    }

    /// The other half: an older version must survive a file a newer one
    /// wrote, which holds only while `PersistedState` stays free of
    /// `deny_unknown_fields`. Downgrades are the case nobody tests by hand.
    #[test]
    fn keys_from_a_newer_version_are_ignored() {
        let with_a_key_we_do_not_know =
            "device_id = 42\nmic_name = \"My-Mic\"\nthreshold = 65\nnoise_model = \"deep\"\n";
        let state: PersistedState =
            toml::from_str(with_a_key_we_do_not_know).expect("an unknown key must not fail load");
        assert_eq!(state.mic_name, "My-Mic");
        assert_eq!(state.threshold, 65);
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
