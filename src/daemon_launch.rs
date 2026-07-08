//! Spawning the daemon and bootstrapping a connection — shared by the GUI
//! launch path and the headless CLI.

use spectral_lib::ipc::{
    DaemonClient, DaemonCommand, DaemonResponse, DaemonState, wait_for_daemon_exit,
};
use std::error::Error;
use std::process::{Command, Stdio};

/// Spawn `spectral-daemon` from the current executable's directory, logging
/// to `$XDG_RUNTIME_DIR/spectral-daemon.log`.
pub fn start_daemon() -> Result<(), Box<dyn Error>> {
    let current_exe = std::env::current_exe()?;
    let daemon_dir = current_exe
        .parent()
        .ok_or("Failed to determine daemon directory (executable might be at root)")?;
    let daemon_path = daemon_dir.join("spectral-daemon");

    let log_path = std::env::var("XDG_RUNTIME_DIR")
        .or_else(|_| std::env::var("HOME").map(|h| format!("{}/tmp", h)))
        .unwrap_or_else(|_| "/tmp".to_string());

    let log_file = std::path::PathBuf::from(log_path).join("spectral-daemon.log");
    let log = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&log_file)?;

    Command::new(&daemon_path)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .spawn()?;

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

/// Connect to the daemon (spawning one if needed) and fetch its state,
/// restarting a daemon whose state no longer parses (an old version).
/// Returns the client so a follow-up command can reuse the connection.
pub fn connect_and_fetch_state() -> Result<(DaemonClient, DaemonState), Box<dyn Error>> {
    let mut client = connect_or_spawn()?;

    match client.send_command(DaemonCommand::GetState) {
        Ok(DaemonResponse::State(state)) => Ok((client, state)),
        Ok(_) => Err("Unexpected response from daemon".into()),
        Err(e) => {
            eprintln!(
                "Failed to communicate with daemon (possibly old version), restarting: {}",
                e
            );
            let _ = client.send_command(DaemonCommand::Shutdown);
            wait_for_daemon_exit()?;
            start_daemon()?;

            let mut new_client = DaemonClient::connect_with_retry()?;
            match new_client.send_command(DaemonCommand::GetState) {
                Ok(DaemonResponse::State(state)) => Ok((new_client, state)),
                _ => Err("Failed to get daemon state after restart".into()),
            }
        }
    }
}
