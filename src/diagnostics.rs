//! The dump behind the error modal's second card: what a bug report needs
//! that the reporter should not have to go hunting for.
//!
//! Everything here is state this process already holds. Probing outward was
//! considered and dropped: a wrapped binary has no PATH worth the name, so
//! `pipewire --version` would fail exactly in the packaging situations where
//! the answer is most interesting, and `/etc/os-release` is not in the
//! Flatpak sandbox at all. A dump that lies under those conditions is worse
//! than one that stays quiet about them.

use spectral_lib::platform::ipc::{DaemonState, PROTOCOL_VERSION};
use spectral_lib::state::{AudioDevice, Status};
use std::path::Path;

const ISSUE_URL: &str = "https://codeberg.org/mmc/spectral/issues";

/// Render the paste-ready dump.
///
/// `daemon_state` is the newest snapshot the poll managed to take, not a
/// fresh fetch — an unreachable daemon is precisely when this text is worth
/// reading, so a stale snapshot goes out under a "last known" label rather
/// than being withheld.
///
/// `log_path` and `config_path` are `None` when the runtime dir and the
/// config base respectively could not be resolved, which is itself the most
/// useful thing the dump can say: naming a path that was never opened would
/// send the reporter hunting for a file that does not exist.
pub fn build_diagnostics(
    daemon_state: Option<&DaemonState>,
    devices: &[AudioDevice],
    log_path: Option<&Path>,
    config_path: Option<&Path>,
    flatpak: bool,
) -> String {
    let mut lines = vec![
        format!("spectral version: {}", env!("CARGO_PKG_VERSION")),
        format!("protocol (gui): {PROTOCOL_VERSION}"),
        format!("packaging: {}", if flatpak { "flatpak" } else { "native" }),
        match log_path {
            Some(path) => format!("daemon log: {}", path.display()),
            None => "daemon log: unavailable (XDG_RUNTIME_DIR is not set to an absolute path)"
                .to_string(),
        },
        match config_path {
            Some(path) => format!("config: {}", path.display()),
            None => "config: unavailable (no valid XDG config base or HOME)".to_string(),
        },
        String::new(),
    ];

    match daemon_state {
        None => lines.push("daemon: unreachable".to_string()),
        Some(state) => {
            lines.push("last known daemon state:".to_string());
            lines.push(format!("  protocol (daemon): {}", state.protocol_version));
            lines.push(format!("  status: {}", describe_status(state.status)));
            lines.push(format!(
                "  device: {}",
                describe_device(state.device_id, devices)
            ));
            if let Some(node_name) = &state.device_node_name {
                lines.push(format!("  device node: {node_name}"));
            }
            lines.push(format!("  mic name: {}", state.mic_name));
            lines.push(format!("  threshold: {}%", state.threshold));
            lines.push(format!("  rumble filter: {}", on_off(state.rumble_filter)));
            lines.push(format!("  auto level: {}", on_off(state.auto_level)));
            lines.push(format!(
                "  playing original: {}",
                yes_no(state.is_playing_original)
            ));
            lines.push(format!(
                "  playing filtered: {}",
                yes_no(state.is_playing_filtered)
            ));
            lines.push(format!(
                "  daemon last error: {}",
                state.last_error.as_deref().unwrap_or("none")
            ));
        }
    }

    lines.push(String::new());
    lines.push(format!("Report issues at {ISSUE_URL}"));
    if log_path.is_some() {
        lines.push("Attach the daemon log named above.".to_string());
    }
    lines.join("\n")
}

fn describe_status(status: Status) -> &'static str {
    match status {
        Status::Active => "Active",
        Status::Pending => "Pending",
        Status::Inactive => "Inactive",
    }
}

fn on_off(value: bool) -> &'static str {
    if value { "on" } else { "off" }
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

/// A bare PipeWire id means nothing to a reader, so it is resolved against
/// the list the window is currently showing. An id that resolves to nothing
/// is not a failure to report — it is the report.
fn describe_device(device_id: Option<u32>, devices: &[AudioDevice]) -> String {
    let Some(id) = device_id else {
        return "none selected".to_string();
    };

    match devices.iter().find(|device| device.id == id) {
        Some(device) => format!("{id} — {} ({}ch)", device.name, device.channels),
        None => format!("{id} (not in the current device list)"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(id: u32, name: &str, channels: u32) -> AudioDevice {
        AudioDevice {
            id,
            name: name.to_string(),
            channels,
        }
    }

    fn state() -> DaemonState {
        DaemonState {
            protocol_version: PROTOCOL_VERSION,
            status: Status::Active,
            device_id: Some(47),
            device_node_name: Some("alsa_input.usb-EVO4".to_string()),
            mic_name: "Spectral-Mic".to_string(),
            threshold: 65,
            rumble_filter: true,
            auto_level: false,
            is_playing_original: false,
            is_playing_filtered: true,
            last_error: None,
        }
    }

    fn render(daemon_state: Option<&DaemonState>, devices: &[AudioDevice]) -> String {
        build_diagnostics(
            daemon_state,
            devices,
            Some(Path::new("/run/user/1000/spectral-daemon.log")),
            Some(Path::new("/home/user/.config/spectral/state.toml")),
            false,
        )
    }

    #[test]
    fn unreachable_daemon_renders_the_fallback_line() {
        let dump = render(None, &[]);

        assert!(dump.contains("daemon: unreachable"));
        assert!(!dump.contains("last known daemon state"));
    }

    #[test]
    fn snapshot_renders_status_settings_and_playback_flags() {
        let dump = render(Some(&state()), &[device(47, "EVO4 Mic 1", 1)]);

        assert!(dump.contains("last known daemon state:"));
        assert!(dump.contains("  protocol (daemon): 2"));
        assert!(dump.contains("  status: Active"));
        assert!(dump.contains("  device node: alsa_input.usb-EVO4"));
        assert!(dump.contains("  mic name: Spectral-Mic"));
        assert!(dump.contains("  threshold: 65%"));
        assert!(dump.contains("  rumble filter: on"));
        assert!(dump.contains("  auto level: off"));
        assert!(dump.contains("  playing original: no"));
        assert!(dump.contains("  playing filtered: yes"));
    }

    #[test]
    fn selected_device_resolves_name_and_channels_from_the_list() {
        let devices = [device(12, "Other", 2), device(47, "EVO4 Mic 1", 1)];

        let dump = render(Some(&state()), &devices);

        assert!(dump.contains("  device: 47 — EVO4 Mic 1 (1ch)"));
    }

    #[test]
    fn device_absent_from_the_list_is_marked_stale() {
        let dump = render(Some(&state()), &[device(12, "Other", 2)]);

        assert!(dump.contains("  device: 47 (not in the current device list)"));
    }

    #[test]
    fn no_selected_device_renders_none_even_with_an_empty_list() {
        let mut snapshot = state();
        snapshot.device_id = None;
        snapshot.device_node_name = None;

        let dump = render(Some(&snapshot), &[]);

        assert!(dump.contains("  device: none selected"));
        assert!(!dump.contains("device node:"));
    }

    #[test]
    fn packaging_line_says_flatpak_or_native() {
        for (flatpak, expected) in [(true, "packaging: flatpak"), (false, "packaging: native")] {
            let dump = build_diagnostics(
                None,
                &[],
                Some(Path::new("/run/user/1000/spectral-daemon.log")),
                Some(Path::new("/tmp/state.toml")),
                flatpak,
            );

            assert!(dump.contains(expected), "flatpak={flatpak}");
        }
    }

    #[test]
    fn daemon_last_error_renders_text_or_none() {
        for (last_error, expected) in [
            (None, "  daemon last error: none"),
            (
                Some("No input device selected".to_string()),
                "  daemon last error: No input device selected",
            ),
        ] {
            let mut snapshot = state();
            snapshot.last_error = last_error;

            let dump = render(Some(&snapshot), &[]);

            assert!(dump.contains(expected), "expected {expected}");
        }
    }

    #[test]
    fn paths_versions_and_the_reporting_footer_are_always_present() {
        for daemon_state in [None, Some(state())] {
            let dump = render(daemon_state.as_ref(), &[]);

            assert!(dump.contains(&format!("spectral version: {}", env!("CARGO_PKG_VERSION"))));
            assert!(dump.contains("protocol (gui): 2"));
            assert!(dump.contains("daemon log: /run/user/1000/spectral-daemon.log"));
            assert!(dump.contains("config: /home/user/.config/spectral/state.toml"));
            // The modal shows no reporting advice of its own, so the dump has
            // to be self-explanatory once pasted.
            assert!(dump.contains(&format!("Report issues at {ISSUE_URL}")));
            assert!(dump.ends_with("Attach the daemon log named above."));
        }
    }

    /// No runtime dir means no log was ever opened. Naming a path anyway
    /// would send the reporter after a file that does not exist, and telling
    /// them to attach it would compound that.
    #[test]
    fn an_unresolvable_log_path_is_reported_as_unavailable() {
        let dump = build_diagnostics(
            Some(&state()),
            &[],
            None,
            Some(Path::new("/home/user/.config/spectral/state.toml")),
            false,
        );

        assert!(
            dump.contains(
                "daemon log: unavailable (XDG_RUNTIME_DIR is not set to an absolute path)"
            )
        );
        assert!(!dump.contains("Attach the daemon log"));
        assert!(dump.ends_with(&format!("Report issues at {ISSUE_URL}")));
    }

    /// Same reasoning one line down: with no valid config base the daemon
    /// declines to persist rather than writing somewhere world-writable, so
    /// there is no config file to name.
    #[test]
    fn an_unresolvable_config_path_is_reported_as_unavailable() {
        let dump = build_diagnostics(
            Some(&state()),
            &[],
            Some(Path::new("/run/user/1000/spectral-daemon.log")),
            None,
            false,
        );

        assert!(dump.contains("config: unavailable (no valid XDG config base or HOME)"));
        // The log is resolvable here, so the dump still asks for it.
        assert!(dump.ends_with("Attach the daemon log named above."));
    }
}
