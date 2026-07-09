use crate::state::Status;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::time::{Duration, Instant};

const DAEMON_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const DAEMON_EXIT_TIMEOUT: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(50);

mod pid_file {
    use std::fs;
    use std::io::{self, Read, Write};
    use std::path::Path;

    pub fn read_pid(path: &Path) -> Option<u32> {
        fs::File::open(path)
            .and_then(|mut f| {
                let mut contents = String::new();
                f.read_to_string(&mut contents)?;
                Ok(contents)
            })
            .ok()
            .and_then(|s| s.trim().parse().ok())
    }

    pub fn write_pid(path: &Path) -> io::Result<()> {
        let mut file = fs::File::create(path)?;
        write!(file, "{}", std::process::id())
    }

    pub fn is_process_running(pid: u32) -> bool {
        Path::new(&format!("/proc/{}", pid)).exists()
    }

    pub fn cleanup(path: &Path) {
        let _ = fs::remove_file(path);
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub enum DaemonCommand {
    /// `None` fields mean "keep the daemon's current setting"; the device is
    /// then resolved from the remembered node name (ids are per-instance).
    Enable {
        device_id: Option<u32>,
        mic_name: Option<String>,
        threshold: Option<i32>,
        voice_smoothing: Option<bool>,
    },
    Disable,
    GetState,
    Shutdown,
    StartRecording,
    StopRecording,
    PlayOriginal,
    StopOriginal,
    PlayFiltered,
    StopFiltered,
}

/// Bump whenever the wire format changes meaning. serde fills missing
/// `Option` fields with `None` even without `#[serde(default)]`, so adding
/// an optional field never makes a new client reject an old daemon's state;
/// this required field is the explicit trip-wire that sends clients down
/// their restart-and-upgrade path.
pub const PROTOCOL_VERSION: u32 = 2;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DaemonState {
    /// Must equal [`PROTOCOL_VERSION`]; absent (pre-0.4 daemons) or
    /// different means the daemon predates this client.
    pub protocol_version: u32,
    pub status: Status,
    pub device_id: Option<u32>,
    pub device_node_name: Option<String>,
    pub mic_name: String,
    pub threshold: i32,
    /// Defaulted so a pre-0.5 daemon's payload still parses and reaches the
    /// version check above instead of dying as a parse error.
    #[serde(default)]
    pub voice_smoothing: bool,
    pub is_playing_original: bool,
    pub is_playing_filtered: bool,
    pub last_error: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum DaemonResponse {
    Ok,
    State(DaemonState),
    Error(String),
}

fn runtime_dir() -> Result<PathBuf, std::io::Error> {
    std::env::var("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "XDG_RUNTIME_DIR is not set; cannot locate the daemon socket",
            )
        })
}

fn get_socket_path() -> Result<PathBuf, std::io::Error> {
    Ok(runtime_dir()?.join("spectral-daemon.sock"))
}

fn get_pid_file_path() -> Result<PathBuf, std::io::Error> {
    Ok(runtime_dir()?.join("spectral-daemon.pid"))
}

/// Wait for a previously running daemon to exit. Shutdown terminates the
/// process without running Drop, so the socket file lingers; the pid file's
/// process going away — the same condition `bind` checks — is the signal.
pub fn wait_for_daemon_exit() -> Result<(), std::io::Error> {
    let Some(pid) = pid_file::read_pid(&get_pid_file_path()?) else {
        return Ok(());
    };
    let deadline = Instant::now() + DAEMON_EXIT_TIMEOUT;
    while pid_file::is_process_running(pid) {
        if Instant::now() >= deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("daemon (pid {pid}) did not exit within {DAEMON_EXIT_TIMEOUT:?}"),
            ));
        }
        std::thread::sleep(POLL_INTERVAL);
    }
    Ok(())
}

pub struct DaemonClient {
    stream: UnixStream,
}

impl DaemonClient {
    pub fn connect() -> Result<Self, std::io::Error> {
        let socket_path = get_socket_path()?;
        let stream = UnixStream::connect(socket_path)?;
        Ok(Self { stream })
    }

    /// Connect, retrying while a freshly spawned daemon starts up; ENOENT
    /// (socket not yet bound) and ECONNREFUSED (stale socket file) are both
    /// normal in that window.
    pub fn connect_with_retry() -> Result<Self, std::io::Error> {
        let deadline = Instant::now() + DAEMON_CONNECT_TIMEOUT;
        loop {
            match Self::connect() {
                Ok(client) => return Ok(client),
                Err(e) if Instant::now() >= deadline => return Err(e),
                Err(_) => std::thread::sleep(POLL_INTERVAL),
            }
        }
    }

    pub fn send_command(
        &mut self,
        command: DaemonCommand,
    ) -> Result<DaemonResponse, std::io::Error> {
        let json = serde_json::to_string(&command)?;
        writeln!(self.stream, "{}", json)?;
        self.stream.flush()?;

        let mut reader = BufReader::new(&self.stream);
        let mut response_line = String::new();
        reader.read_line(&mut response_line)?;

        serde_json::from_str(&response_line)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }
}

pub struct DaemonServer {
    listener: UnixListener,
    socket_path: PathBuf,
    pid_file_path: PathBuf,
}

impl DaemonServer {
    pub fn bind() -> Result<Self, std::io::Error> {
        let socket_path = get_socket_path()?;
        let pid_file_path = get_pid_file_path()?;

        // Check if another daemon is already running via PID file
        if let Some(existing_pid) = pid_file::read_pid(&pid_file_path) {
            if pid_file::is_process_running(existing_pid) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AddrInUse,
                    format!("Another daemon is already running (PID {})", existing_pid),
                ));
            }
            pid_file::cleanup(&pid_file_path);
        }

        let listener = match UnixListener::bind(&socket_path) {
            Ok(listener) => listener,
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                std::fs::remove_file(&socket_path)?;
                UnixListener::bind(&socket_path)?
            }
            Err(e) => return Err(e),
        };

        pid_file::write_pid(&pid_file_path)?;

        listener.set_nonblocking(true)?;

        Ok(Self {
            listener,
            socket_path,
            pid_file_path,
        })
    }

    pub fn accept(&self) -> Result<DaemonConnection, std::io::Error> {
        let (stream, _) = self.listener.accept()?;
        Ok(DaemonConnection { stream })
    }
}

impl Drop for DaemonServer {
    fn drop(&mut self) {
        pid_file::cleanup(&self.pid_file_path);
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

pub struct DaemonConnection {
    stream: UnixStream,
}

impl DaemonConnection {
    pub fn read_command(&mut self) -> Result<DaemonCommand, std::io::Error> {
        self.stream
            .set_read_timeout(Some(Duration::from_secs(30)))?;

        let mut reader = BufReader::new(&self.stream);
        let mut line = String::new();
        reader.read_line(&mut line)?;

        if line.trim().is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "Connection closed",
            ));
        }

        serde_json::from_str(&line).map_err(|e| {
            eprintln!("[Daemon] Failed to parse command: {}", e);
            std::io::Error::new(std::io::ErrorKind::InvalidData, e)
        })
    }

    pub fn send_response(&mut self, response: DaemonResponse) -> Result<(), std::io::Error> {
        let json = serde_json::to_string(&response)?;
        writeln!(self.stream, "{}", json)?;
        self.stream.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A 0.3.0 daemon's GetState payload: no protocol_version,
    // device_node_name, and (pre-0.3) last_error.
    const OLD_STATE: &str = r#"{"status":"Active","device_id":42,"mic_name":"Spectral-Mic","threshold":80,"is_playing_original":false,"is_playing_filtered":false}"#;

    #[test]
    fn missing_option_fields_parse_as_none() {
        let with_version = format!(r#"{{"protocol_version":1,{}"#, &OLD_STATE[1..]);
        let state: DaemonState =
            serde_json::from_str(&with_version).expect("optional fields may be absent");
        assert_eq!(state.device_node_name, None);
        assert_eq!(state.last_error, None);
        assert!(!state.voice_smoothing);
    }

    #[test]
    fn missing_protocol_version_fails_to_parse() {
        assert!(serde_json::from_str::<DaemonState>(OLD_STATE).is_err());
    }
}
