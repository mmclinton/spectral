//! Spawning the daemon and bootstrapping a connection — shared by the GUI
//! launch path and the headless CLI.

use spectral_lib::platform::ipc::{
    DaemonClient, DaemonCommand, DaemonResponse, DaemonState, PROTOCOL_VERSION,
    wait_for_daemon_exit,
};
use std::error::Error;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Where the daemon's stdout and stderr land — `$XDG_RUNTIME_DIR`, falling
/// back to `$HOME/tmp` and then `/tmp`. The GUI reports this path in its
/// diagnostics dump, so the two must not drift apart.
pub fn daemon_log_path() -> PathBuf {
    let dir = std::env::var("XDG_RUNTIME_DIR")
        .or_else(|_| std::env::var("HOME").map(|h| format!("{h}/tmp")))
        .unwrap_or_else(|_| "/tmp".to_string());

    PathBuf::from(dir).join("spectral-daemon.log")
}

/// Spawn `spectral-daemon` from the current executable's directory, logging
/// to [`daemon_log_path`].
pub fn start_daemon() -> Result<(), Box<dyn Error>> {
    let current_exe = std::env::current_exe()?;
    let daemon_dir = current_exe
        .parent()
        .ok_or("Failed to determine daemon directory (executable might be at root)")?;
    let daemon_path = daemon_dir.join("spectral-daemon");

    let log_file = daemon_log_path();
    let log = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&log_file)?;

    let mut child = Command::new(&daemon_path)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .spawn()?;

    // The daemon normally outlives us, but if it crashes or is shut down
    // while the GUI runs, a dropped Child would linger as a zombie in our
    // process table. Reap it off-thread; process exit does not wait for
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

/// Connect to the daemon (spawning one if needed) and fetch its state,
/// restarting a daemon that answers with an old protocol — or not at all.
/// Returns the client so a follow-up command can reuse the connection.
pub fn connect_and_fetch_state() -> Result<(DaemonClient, DaemonState), Box<dyn Error>> {
    let mut client = connect_or_spawn()?;

    match client.send_command(DaemonCommand::GetState) {
        Ok(DaemonResponse::State(state)) if state.protocol_version == PROTOCOL_VERSION => {
            Ok((client, state))
        }
        Ok(DaemonResponse::State(_)) => {
            eprintln!("Daemon speaks an old protocol, restarting it");
            restart_and_refetch(client)
        }
        Ok(_) => Err("Unexpected response from daemon".into()),
        Err(e) => {
            eprintln!("Failed to communicate with daemon (possibly old version), restarting: {e}");
            restart_and_refetch(client)
        }
    }
}

fn restart_and_refetch(
    mut old_client: DaemonClient,
) -> Result<(DaemonClient, DaemonState), Box<dyn Error>> {
    let _ = old_client.send_command(DaemonCommand::Shutdown);
    drop(old_client);
    wait_for_daemon_exit()?;
    start_daemon()?;

    let mut client = DaemonClient::connect_with_retry()?;
    match client.send_command(DaemonCommand::GetState) {
        Ok(DaemonResponse::State(state)) => Ok((client, state)),
        _ => Err("Failed to get daemon state after restart".into()),
    }
}
