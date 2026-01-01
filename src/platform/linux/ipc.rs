use crate::state::Status;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;

/// PID file management for single-instance daemon
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
        // On Linux, check if /proc/<pid> exists
        Path::new(&format!("/proc/{}", pid)).exists()
    }

    pub fn cleanup(path: &Path) {
        let _ = fs::remove_file(path);
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub enum DaemonCommand {
    Enable {
        device_id: u32,
        mic_name: String,
        threshold: i32,
    },
    Disable,
    UpdateThreshold(i32),
    UpdateMicName(String),
    UpdateDevice(u32),
    GetState,
    Shutdown,
    StartRecording,
    StopRecording,
    PlayOriginal,
    StopOriginal,
    PlayFiltered,
    StopFiltered,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DaemonState {
    pub status: Status,
    pub device_id: Option<u32>,
    pub mic_name: String,
    pub threshold: i32,
    pub is_recording: bool,
    pub has_recording: bool,
    pub is_playing_original: bool,
    pub is_playing_filtered: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum DaemonResponse {
    Ok,
    State(DaemonState),
    Error(String),
}

pub fn get_socket_path() -> PathBuf {
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| {
        format!(
            "/tmp/runtime-{}",
            std::env::var("USER").unwrap_or_else(|_| "user".to_string())
        )
    });
    PathBuf::from(runtime_dir).join("spectral-daemon.sock")
}

fn get_pid_file_path() -> PathBuf {
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| {
        format!(
            "/tmp/runtime-{}",
            std::env::var("USER").unwrap_or_else(|_| "user".to_string())
        )
    });
    PathBuf::from(runtime_dir).join("spectral-daemon.pid")
}

pub struct DaemonClient {
    stream: UnixStream,
}

impl DaemonClient {
    pub fn connect() -> Result<Self, std::io::Error> {
        let socket_path = get_socket_path();
        let stream = UnixStream::connect(socket_path)?;
        Ok(Self { stream })
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
    _pid_file_path: PathBuf,
}

impl DaemonServer {
    pub fn bind() -> Result<Self, std::io::Error> {
        let socket_path = get_socket_path();
        let pid_file_path = get_pid_file_path();

        // Check if another daemon is already running via PID file
        if let Some(existing_pid) = pid_file::read_pid(&pid_file_path) {
            if pid_file::is_process_running(existing_pid) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AddrInUse,
                    format!("Another daemon is already running (PID {})", existing_pid),
                ));
            }
            // Stale PID file, clean it up
            pid_file::cleanup(&pid_file_path);
        }

        // Try to bind, handle stale socket files
        let listener = match UnixListener::bind(&socket_path) {
            Ok(listener) => listener,
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                // Socket file exists but no daemon running (we checked PID above)
                // Safe to remove the stale socket
                std::fs::remove_file(&socket_path)?;
                UnixListener::bind(&socket_path)?
            }
            Err(e) => return Err(e),
        };

        // Write our PID file
        pid_file::write_pid(&pid_file_path)?;

        // Set non-blocking mode for graceful shutdown
        listener.set_nonblocking(true)?;

        Ok(Self {
            listener,
            _pid_file_path: pid_file_path,
        })
    }

    pub fn accept(&self) -> Result<DaemonConnection, std::io::Error> {
        let (stream, _) = self.listener.accept()?;
        Ok(DaemonConnection { stream })
    }
}

impl Drop for DaemonServer {
    fn drop(&mut self) {
        // Clean up PID file on shutdown
        pid_file::cleanup(&self._pid_file_path);
        // Clean up socket file
        let _ = std::fs::remove_file(get_socket_path());
    }
}

pub struct DaemonConnection {
    stream: UnixStream,
}

impl DaemonConnection {
    pub fn read_command(&mut self) -> Result<DaemonCommand, std::io::Error> {
        use std::time::Duration;

        // Set read timeout to prevent indefinite blocking
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
