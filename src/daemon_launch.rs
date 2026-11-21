//! Spawning the daemon and bootstrapping a connection — shared by the GUI
//! launch path and the headless CLI.

use spectral_lib::platform::ipc::{
    DaemonClient, DaemonCommand, DaemonResponse, DaemonState, PROTOCOL_VERSION, SPECTRAL_VERSION,
    daemon_log_path, wait_for_daemon_exit,
};
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

/// A connection to a daemon of this build, and the state it reported.
pub struct Connection {
    pub client: DaemonClient,
    pub state: DaemonState,
}

/// Connect to the daemon (spawning one if needed) and fetch its state,
/// replacing a daemon of another protocol or version, or one that does not
/// answer. The connection stays open for a follow-up command.
pub fn connect_and_fetch_state() -> Result<Connection, Box<dyn Error>> {
    let mut client = connect_or_spawn()?;

    match client.send_command(DaemonCommand::GetState) {
        Ok(DaemonResponse::State(state)) => match restart_reason(&state) {
            None => Ok(Connection { client, state }),
            Some(reason) => {
                eprintln!("{reason}; restarting it");
                restart_and_refetch(client)
            }
        },
        Ok(_) => Err("Unexpected response from daemon".into()),
        Err(e) => {
            eprintln!("Failed to communicate with daemon (possibly old version), restarting: {e}");
            restart_and_refetch(client)
        }
    }
}

fn restart_and_refetch(mut old_client: DaemonClient) -> Result<Connection, Box<dyn Error>> {
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
            Ok(Connection { client, state })
        }
        Ok(DaemonResponse::Error(e)) => {
            Err(format!("Daemon refused GetState after restart: {e}").into())
        }
        Ok(DaemonResponse::Ok) => Err("Unexpected response from daemon after restart".into()),
        Err(e) => Err(format!("Failed to get daemon state after restart: {e}").into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spectral_lib::state::Status;

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
}
