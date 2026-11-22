//! Spawning the daemon and bootstrapping a connection — shared by the GUI
//! launch path and the headless CLI.

use spectral_lib::platform::ipc::{
    DaemonClient, DaemonCommand, DaemonResponse, DaemonState, PROTOCOL_VERSION, SPECTRAL_VERSION,
    daemon_log_path, wait_for_daemon_exit,
};
use spectral_lib::state::Status;
use std::error::Error;
use std::fmt;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

/// Spawn `spectral-daemon` from the current executable's directory, logging
/// to [`daemon_log_path`].
pub fn start_daemon() -> Result<(), Box<dyn Error>> {
    let current_exe = std::env::current_exe()?;
    let daemon_dir = current_exe
        .parent()
        .ok_or("Failed to determine daemon directory (executable might be at root)")?;
    let daemon_path = daemon_dir.join("spectral-daemon");

    let log_file = daemon_log_path()?;
    // Append, never truncate. Two clients can both find no daemon and both
    // spawn one; only one child wins the pid lock, but with O_TRUNC the
    // second client would first wipe what the winner had already logged and
    // then leave the winner's file offset past EOF, padding the rest of the
    // session's output with NULs. Starting the log clean is the winning
    // daemon's job, once it knows it is the winner.
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_file)?;

    // Its own process group, so the terminal that launched this client
    // cannot signal the daemon: Ctrl-C there would otherwise take the
    // virtual mic down with the window, and closing the terminal would
    // SIGHUP it without a clean teardown.
    let mut child = Command::new(&daemon_path)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .process_group(0)
        .spawn()?;

    // The daemon normally outlives this process, but if it crashes or is shut
    // down while the GUI runs, a dropped Child would linger as a zombie in
    // the process table. Reap it off-thread; process exit does not wait for
    // this thread, so short-lived CLI invocations are unaffected.
    std::thread::spawn(move || {
        let _ = child.wait();
    });

    Ok(())
}

/// Connect to a running daemon, spawning one first if none answers.
pub fn connect_or_spawn() -> Result<DaemonClient, Box<dyn Error>> {
    match DaemonClient::connect() {
        Ok(client) => Ok(client),
        Err(_) => {
            start_daemon()?;
            Ok(DaemonClient::connect_with_retry()?)
        }
    }
}

/// Why a running daemon is not this client's own and has to be replaced.
#[derive(Debug, PartialEq, Eq)]
pub enum RestartReason {
    /// It speaks another wire protocol.
    Protocol(u32),
    /// It is another release of Spectral, older or newer.
    Version(String),
    /// It is from before daemons reported their version.
    Unversioned,
}

impl fmt::Display for RestartReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Protocol(daemon) => write!(
                f,
                "the daemon speaks protocol {daemon} and this client {PROTOCOL_VERSION}"
            ),
            Self::Version(daemon) => {
                write!(
                    f,
                    "the daemon is {daemon} and this client {SPECTRAL_VERSION}"
                )
            }
            Self::Unversioned => write!(
                f,
                "the daemon predates version reporting and this client is {SPECTRAL_VERSION}"
            ),
        }
    }
}

/// Whether the daemon that answered with `state` has to be replaced. Any
/// protocol or version but this build's counts, newer as well as older, so
/// a client always ends up with the daemon from its own build, rollbacks
/// included.
pub fn restart_reason(state: &DaemonState) -> Option<RestartReason> {
    if state.protocol_version != PROTOCOL_VERSION {
        return Some(RestartReason::Protocol(state.protocol_version));
    }
    match state.daemon_version.as_deref() {
        None => Some(RestartReason::Unversioned),
        Some(version) if version != SPECTRAL_VERSION => {
            Some(RestartReason::Version(version.to_string()))
        }
        Some(_) => None,
    }
}

/// A connection to a daemon of this build, the state it reported, and the
/// state of the daemon it replaced when there was one that answered.
pub struct Connection {
    pub client: DaemonClient,
    pub state: DaemonState,
    pub replaced: Option<DaemonState>,
}

/// Connect to the daemon (spawning one if needed) and fetch its state,
/// replacing a daemon of another protocol or version, or one that does not
/// answer. The connection stays open for a follow-up command.
pub fn connect_and_fetch_state() -> Result<Connection, Box<dyn Error>> {
    let mut client = connect_or_spawn()?;

    match client.send_command(DaemonCommand::GetState) {
        Ok(DaemonResponse::State(state)) => match restart_reason(&state) {
            None => Ok(Connection {
                client,
                state,
                replaced: None,
            }),
            Some(reason) => {
                eprintln!("warning: {reason}; restarting it");
                restart_and_refetch(client, Some(state))
            }
        },
        Ok(_) => Err("Unexpected response from daemon".into()),
        Err(e) => {
            eprintln!("Failed to communicate with daemon (possibly old version), restarting: {e}");
            restart_and_refetch(client, None)
        }
    }
}

fn restart_and_refetch(
    mut old_client: DaemonClient,
    replaced: Option<DaemonState>,
) -> Result<Connection, Box<dyn Error>> {
    let _ = old_client.send_command(DaemonCommand::Shutdown);
    drop(old_client);
    wait_for_daemon_exit()?;
    start_daemon()?;

    let mut client = DaemonClient::connect_with_retry()?;
    match client.send_command(DaemonCommand::GetState) {
        Ok(DaemonResponse::State(state)) => {
            // Not restarted again: the daemon beside this executable is the
            // only one there is to start, so a mismatch here is a broken
            // install, and a second restart would only repeat it.
            if let Some(reason) = restart_reason(&state) {
                eprintln!(
                    "warning: {reason} even after a restart; the installed daemon does not match this client"
                );
            }
            Ok(Connection {
                client,
                state,
                replaced,
            })
        }
        Ok(DaemonResponse::Error(e)) => {
            Err(format!("Daemon refused GetState after restart: {e}").into())
        }
        Ok(DaemonResponse::Ok) => Err("Unexpected response from daemon after restart".into()),
        Err(e) => Err(format!("Failed to get daemon state after restart: {e}").into()),
    }
}

/// The command that brings the mic back as the replaced daemon had it, or
/// `None` when its mic was off or the new daemon's is already on, which
/// means another client replacing the same daemon got there first. The
/// device goes by its durable node name, with the id only for a snapshot
/// that has none.
fn restore_command(replaced: &DaemonState, current: Status) -> Option<DaemonCommand> {
    if current != Status::Inactive {
        return None;
    }
    match replaced.status {
        Status::Inactive => None,
        Status::Active | Status::Pending => Some(DaemonCommand::Enable {
            device_id: replaced
                .device_id
                .filter(|_| replaced.device_node_name.is_none()),
            device_node_name: replaced.device_node_name.clone(),
            mic_name: Some(replaced.mic_name.clone()),
            threshold: Some(replaced.threshold),
            rumble_filter: Some(replaced.rumble_filter),
            auto_level: Some(replaced.auto_level),
        }),
    }
}

/// Turn the mic back on in the new daemon when the one it replaced had it
/// on, then refresh `connection.state` to match. Does nothing when no daemon
/// was replaced, its mic was off, or the new daemon's is already on.
/// `spectral enable` calls this only when the daemon rejects its own
/// `Enable`: an accepted one brings the mic up, and restoring first would
/// rebuild it twice.
///
/// An error means the mic did not come back. A refresh that fails after the
/// mic is back only warns, leaving the stale state for the GUI's poll to
/// correct.
pub fn restore(connection: &mut Connection) -> Result<(), Box<dyn Error>> {
    let current = connection.state.status;
    let Some(command) = connection
        .replaced
        .as_ref()
        .and_then(|replaced| restore_command(replaced, current))
    else {
        return Ok(());
    };
    match connection.client.send_command(command)? {
        DaemonResponse::Ok => {}
        DaemonResponse::Error(e) => return Err(e.into()),
        DaemonResponse::State(_) => return Err("unexpected response to Enable".into()),
    }
    match connection.client.send_command(DaemonCommand::GetState) {
        Ok(DaemonResponse::State(state)) => connection.state = state,
        Ok(DaemonResponse::Error(e)) => {
            eprintln!("warning: the mic is back on, but the daemon refused GetState: {e}");
        }
        Ok(DaemonResponse::Ok) => {
            eprintln!("warning: the mic is back on, but GetState got an unexpected response");
        }
        Err(e) => eprintln!("warning: the mic is back on, but reading its state failed: {e}"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(protocol_version: u32, daemon_version: Option<&str>) -> DaemonState {
        DaemonState {
            protocol_version,
            daemon_version: daemon_version.map(str::to_string),
            status: Status::Active,
            device_id: Some(42),
            device_node_name: Some("alsa_input.usb-Blue_Yeti-00.analog-stereo".to_string()),
            mic_name: "Spectral-Mic".to_string(),
            threshold: 80,
            rumble_filter: true,
            auto_level: false,
            capture_detached: false,
            is_recording: false,
            is_playing_original: false,
            is_playing_filtered: false,
            last_error: None,
        }
    }

    #[test]
    fn only_a_daemon_of_this_protocol_and_version_is_kept() {
        let other = PROTOCOL_VERSION + 1;
        let cases = [
            (state(PROTOCOL_VERSION, Some(SPECTRAL_VERSION)), None),
            (
                state(PROTOCOL_VERSION, Some("0.0.1")),
                Some(RestartReason::Version("0.0.1".to_string())),
            ),
            (
                state(PROTOCOL_VERSION, Some("999.0.0")),
                Some(RestartReason::Version("999.0.0".to_string())),
            ),
            (
                state(PROTOCOL_VERSION, None),
                Some(RestartReason::Unversioned),
            ),
            (
                state(other, Some(SPECTRAL_VERSION)),
                Some(RestartReason::Protocol(other)),
            ),
            (state(other, None), Some(RestartReason::Protocol(other))),
        ];
        for (daemon, expected) in cases {
            assert_eq!(restart_reason(&daemon), expected, "{daemon:?}");
        }
    }

    #[test]
    fn a_mic_that_was_on_comes_back_with_every_setting() {
        for status in [Status::Active, Status::Pending] {
            let old = DaemonState {
                status,
                mic_name: "Desk-Mic".to_string(),
                threshold: 65,
                rumble_filter: true,
                auto_level: true,
                ..state(PROTOCOL_VERSION, Some("0.0.1"))
            };
            match restore_command(&old, Status::Inactive) {
                Some(DaemonCommand::Enable {
                    device_id,
                    device_node_name,
                    mic_name,
                    threshold,
                    rumble_filter,
                    auto_level,
                }) => {
                    assert_eq!(device_id, None, "the durable node name names the device");
                    assert_eq!(
                        device_node_name.as_deref(),
                        Some("alsa_input.usb-Blue_Yeti-00.analog-stereo")
                    );
                    assert_eq!(mic_name.as_deref(), Some("Desk-Mic"));
                    assert_eq!(threshold, Some(65));
                    assert_eq!(rumble_filter, Some(true));
                    assert_eq!(auto_level, Some(true));
                }
                other => panic!("{status:?} must restore with Enable, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_mic_that_was_off_stays_off() {
        let old = DaemonState {
            status: Status::Inactive,
            ..state(PROTOCOL_VERSION, None)
        };
        assert!(restore_command(&old, Status::Inactive).is_none());
    }

    #[test]
    fn a_mic_another_client_already_turned_on_is_left_alone() {
        let old = state(PROTOCOL_VERSION, None);
        for current in [Status::Active, Status::Pending] {
            assert!(restore_command(&old, current).is_none(), "{current:?}");
        }
    }

    #[test]
    fn a_snapshot_without_a_node_name_restores_by_device_id() {
        let old = DaemonState {
            device_node_name: None,
            ..state(PROTOCOL_VERSION, None)
        };
        match restore_command(&old, Status::Inactive) {
            Some(DaemonCommand::Enable {
                device_id,
                device_node_name,
                ..
            }) => {
                assert_eq!(device_id, Some(42));
                assert_eq!(device_node_name, None);
            }
            other => panic!("must restore with Enable, got {other:?}"),
        }
    }
}
