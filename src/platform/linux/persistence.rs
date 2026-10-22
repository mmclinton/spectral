//! XDG-compliant TOML persistence for daemon settings, plus the on-disk
//! locations of the voice-test recordings.

use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{self, ErrorKind, Write};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

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

/// Absolute, and with no `..` component that could climb back out of the
/// directory the caller means to stay inside.
///
/// Tested by component rather than by substring: `..` appears inside plenty
/// of ordinary names — `~/dotfiles..bak` is a legal directory — and
/// rejecting those sends settings somewhere the user did not configure.
/// Only a whole `..` path element can traverse.
fn is_safe_base(path: &Path) -> bool {
    path.is_absolute() && !path.components().any(|c| c == Component::ParentDir)
}

/// Resolve an XDG base directory, falling back to `$HOME` plus
/// `home_suffix`.
///
/// `var_os` rather than `var`: a path is bytes on Linux, and a non-UTF-8
/// `HOME` is one this code can still join onto — reporting it as "HOME is
/// not set" would be a wrong diagnostic for a directory that exists.
fn safe_base_dir(xdg_var: &str, home_suffix: &[&str]) -> Result<PathBuf, io::Error> {
    match std::env::var_os(xdg_var) {
        // The XDG spec treats an empty value as absent, so fall through
        // without a warning: nothing is wrong for the user to fix.
        Some(value) if value.is_empty() => {}
        Some(value) => {
            let path = PathBuf::from(value);
            if is_safe_base(&path) {
                return Ok(path);
            }
            eprintln!("Warning: {xdg_var} is not a safe absolute path ({path:?}), using fallback");
        }
        None => {}
    }

    let home = PathBuf::from(
        std::env::var_os("HOME")
            .ok_or_else(|| io::Error::new(ErrorKind::NotFound, "HOME is not set"))?,
    );

    if !is_safe_base(&home) {
        return Err(io::Error::new(
            ErrorKind::InvalidInput,
            format!("HOME is not a safe absolute path: {home:?}"),
        ));
    }

    Ok(home_suffix.iter().fold(home, |path, part| path.join(part)))
}

fn get_safe_config_base() -> Result<PathBuf, io::Error> {
    safe_base_dir("XDG_CONFIG_HOME", &[".config"])
}

fn get_safe_state_base() -> Result<PathBuf, io::Error> {
    safe_base_dir("XDG_STATE_HOME", &[".local", "state"])
}

/// Name of the settings file inside the config directory. Public because the
/// man page names the path and a test pins the two together.
pub const SETTINGS_FILE_NAME: &str = "state.toml";

/// Path of the settings file, `$XDG_CONFIG_HOME/spectral/state.toml` (or the
/// `~/.config` fallback).
///
/// There is deliberately no second location. The obvious fallback is a fixed
/// path under `/tmp`, and that is a symlink target on a shared system: every
/// write here either creates a temp file or is read back and trusted
/// (`state.toml`), and neither `File::create` nor `fs::read_to_string` refuses
/// to follow a link another user planted first. `ipc::daemon_log_path`
/// already refuses `/tmp` for the same reason, and the payload here — the
/// settings the daemon acts on — is worth more than the log. A daemon that cannot name a private directory
/// should decline to persist, not persist somewhere world-writable.
pub fn get_config_path() -> Result<PathBuf, io::Error> {
    Ok(get_safe_config_base()?
        .join("spectral")
        .join(SETTINGS_FILE_NAME))
}

/// Directory holding the voice-test recordings, under the XDG state dir
/// (recordings are regenerable audio, not configuration); created on demand.
///
/// Refuses to fall back to `/tmp` for the reason given on
/// [`get_config_path`], which applies with more force here: these are raw
/// microphone captures.
///
/// For the same reason the directory is owner-only rather than whatever the
/// umask allows. A 0755 recordings dir under a 0755 home — the default on
/// Debian and Ubuntu, where NixOS's 0700 home would otherwise mask it —
/// leaves an unprocessed recording of the user's microphone readable by
/// every other local account.
pub fn get_recordings_dir() -> Result<PathBuf, io::Error> {
    let state_base = get_safe_state_base()?;
    // The XDG state dir itself is shared ground, so it keeps the umask;
    // only Spectral's own subtree is narrowed. `mode` applies to the
    // directories this call creates, never to one that already exists.
    fs::create_dir_all(&state_base)?;

    let recordings_dir = state_base.join("spectral").join("recordings");
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&recordings_dir)?;
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
    read_state_from(&get_config_path()?)
}

/// Only a file that is not there means a first run. `Path::exists` would
/// also answer false for one that is there but unreadable, and so would
/// quietly start an existing install over from defaults.
fn read_state_from(path: &Path) -> Result<PersistedState, io::Error> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(PersistedState::default()),
        Err(e) => return Err(e),
    };
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

/// Distinguishes concurrent temp files within one process; the pid alone
/// cannot, because the writers are threads of the same daemon.
static WRITE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Serializes `state` to `path` through a single-writer same-directory temp
/// file and a rename, so a crash or power loss mid-write can never leave a
/// torn half-file behind (rename is atomic within one filesystem on POSIX).
///
/// The temp name has to be unique per write, not per path. The daemon runs
/// one handler thread per connection and saves outside any lock, so two
/// clients enabling at once (GUI Apply and a `spectral enable`) would
/// otherwise share one temp inode: each truncates it and writes at its own
/// offset, and both rename the spliced result over the settings file. The
/// next load fails to parse and silently falls back to defaults.
fn write_state_to(path: &Path, state: &PersistedState) -> Result<(), io::Error> {
    let toml_string =
        toml::to_string(state).map_err(|e| io::Error::new(ErrorKind::InvalidData, e))?;

    let tmp = path.with_extension(format!(
        "toml.{}.{}.tmp",
        std::process::id(),
        WRITE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));

    let result = write_and_sync(&tmp, toml_string.as_bytes()).and_then(|()| fs::rename(&tmp, path));
    if result.is_err() {
        // Nothing else will ever name this file again.
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Write `bytes` to a fresh file and get them onto the disk before the
/// caller renames it into place.
///
/// Rename is atomic for the directory entry only — it orders no data
/// blocks. Without the sync a power cut just after the rename can leave the
/// settings file present but zero-length or NUL-padded, which reads back as
/// a parse failure and resets the user's settings: the very outcome the
/// rename is here to prevent. The payload is a few hundred bytes written
/// once per Enable, so the flush costs nothing worth saving.
fn write_and_sync(path: &Path, bytes: &[u8]) -> Result<(), io::Error> {
    let mut file = File::create(path)?;
    file.write_all(bytes)?;
    file.sync_all()
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
    fn a_missing_settings_file_loads_defaults_but_an_unreadable_one_is_an_error() {
        let dir = std::env::temp_dir().join(format!(
            "spectral_persistence_unreadable_{}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();

        let missing = read_state_from(&dir.join("state.toml")).unwrap();
        assert_eq!(missing.mic_name, PersistedState::default().mic_name);

        // A file where a directory should be fails with ENOTDIR whoever runs
        // the test; permission bits would not stop root.
        let blocker = dir.join("blocker");
        fs::write(&blocker, "").unwrap();
        assert!(read_state_from(&blocker.join("state.toml")).is_err());

        let _ = fs::remove_dir_all(&dir);
    }

    /// Temp files are named per write, so this cannot look for one fixed
    /// name — it asserts the directory holds no `.tmp` leftovers at all.
    fn temp_files_in(dir: &Path) -> Vec<PathBuf> {
        fs::read_dir(dir)
            .expect("the test directory must be readable")
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .filter(|p| p.to_string_lossy().ends_with(".tmp"))
            .collect()
    }

    #[test]
    fn save_replaces_the_file_atomically_and_cleans_up_its_temp() {
        let dir = std::env::temp_dir().join(format!(
            "spectral_persistence_atomic_save_{}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.toml");

        write_state_to(&path, &PersistedState::default()).unwrap();
        let loaded: PersistedState = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(loaded.mic_name, "Spectral-Mic");
        assert!(
            temp_files_in(&dir).is_empty(),
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

    /// Concurrent saves are reachable in the daemon: one handler thread per
    /// connection, and `save_state` runs under no lock. Whichever write lands
    /// last, the file must be one writer's whole output — never two spliced
    /// together, which is what a shared temp name produced.
    #[test]
    fn concurrent_saves_never_splice_two_writers_together() {
        let dir = std::env::temp_dir().join(format!(
            "spectral_persistence_concurrent_save_{}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.toml");

        let thresholds: Vec<i32> = (0..8).map(|i| 10 + i * 10).collect();
        std::thread::scope(|scope| {
            for &threshold in &thresholds {
                let path = path.clone();
                scope.spawn(move || {
                    let state = PersistedState {
                        threshold,
                        // Vary the length too: equal-length payloads could
                        // splice into something that still parses.
                        mic_name: "M".repeat(threshold as usize / 10),
                        ..PersistedState::default()
                    };
                    write_state_to(&path, &state).expect("a concurrent save must succeed");
                });
            }
        });

        let loaded: PersistedState = toml::from_str(&fs::read_to_string(&path).unwrap())
            .expect("the surviving file must be valid TOML");
        assert!(
            thresholds.contains(&loaded.threshold),
            "threshold {} is not one any writer wrote",
            loaded.threshold
        );
        assert_eq!(
            loaded.mic_name.len(),
            loaded.threshold as usize / 10,
            "name and threshold came from different writers"
        );
        assert!(temp_files_in(&dir).is_empty(), "temp files must not leak");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_base_dir_is_safe_unless_it_is_relative_or_can_climb_out() {
        for accepted in [
            "/home/mc/.config",
            // `..` inside a name is not a parent-dir component; rejecting
            // these silently redirected the user's settings elsewhere.
            "/home/mc/dotfiles..bak/config",
            "/home/mc/..hidden",
            "/srv/a..b/c",
        ] {
            assert!(is_safe_base(Path::new(accepted)), "{accepted} must be safe");
        }

        for rejected in [
            ".config",
            "relative/path",
            "/home/mc/../root/.config",
            "/..",
            "",
        ] {
            assert!(
                !is_safe_base(Path::new(rejected)),
                "{rejected} must be refused"
            );
        }
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
