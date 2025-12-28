use crate::state::Status;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;

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
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DaemonState {
    pub status: Status,
    pub device_id: Option<u32>,
    pub mic_name: String,
    pub threshold: i32,
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
}

impl DaemonServer {
    pub fn bind() -> Result<Self, std::io::Error> {
        let socket_path = get_socket_path();

        // Try to bind first, handle conflicts if they occur
        let listener = match UnixListener::bind(&socket_path) {
            Ok(listener) => listener,
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                // Socket exists, try to remove and retry once
                std::fs::remove_file(&socket_path)?;
                UnixListener::bind(&socket_path)?
            }
            Err(e) => return Err(e),
        };

        // Set non-blocking mode for graceful shutdown
        listener.set_nonblocking(true)?;

        Ok(Self { listener })
    }

    pub fn accept(&self) -> Result<DaemonConnection, std::io::Error> {
        let (stream, _) = self.listener.accept()?;
        Ok(DaemonConnection { stream })
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
