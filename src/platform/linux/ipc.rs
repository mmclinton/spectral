//! Unix-socket IPC between clients (GUI, CLI) and the daemon: one JSON
//! [`DaemonCommand`] per line, answered by one JSON [`DaemonResponse`] per
//! line, on `$XDG_RUNTIME_DIR/spectral-daemon.sock`.

use crate::state::Status;
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions, TryLockError};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::time::{Duration, Instant};

const DAEMON_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const DAEMON_EXIT_TIMEOUT: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Ceiling on waiting for the other side of a connection, applied by both
/// ends so neither outlasts the other.
///
/// A legitimately slow command (Enable's ~5 s pipeline handshake, possibly
/// queued behind another client's) stays well inside it; only a genuinely
/// wedged peer trips it. Without it the GUI's event-loop thread would
/// block forever on a dead daemon, and a silent client would pin its
/// handler thread just as long.
const CONNECTION_READ_TIMEOUT: Duration = Duration::from_secs(30);

mod pid_file {
    use std::fs;
    use std::io::{self, Read, Seek, SeekFrom, Write};
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

    /// Rewrites `file` (already opened and locked by the caller) with the
    /// calling process pid. Opened without truncate so a losing racer can't clobber the
    /// winner's pid before the lock decides ownership; truncation happens
    /// here, under the lock.
    pub fn write_pid_to(mut file: &fs::File) -> io::Result<()> {
        file.set_len(0)?;
        file.seek(SeekFrom::Start(0))?;
        write!(file, "{}", std::process::id())
    }

    pub fn is_process_running(pid: u32) -> bool {
        Path::new(&format!("/proc/{pid}")).exists()
    }

    pub fn cleanup(path: &Path) {
        let _ = fs::remove_file(path);
    }
}

/// A request a client sends the daemon; each gets one [`DaemonResponse`].
#[derive(Debug, Serialize, Deserialize)]
pub enum DaemonCommand {
    /// Brings the virtual mic up. `None` fields mean "keep the daemon's
    /// current setting"; with no device named at all, the device is
    /// resolved from the remembered node name (ids are per-instance).
    Enable {
        /// PipeWire id of the capture device to use.
        device_id: Option<u32>,
        /// The capture device's durable `node.name`. Preferred over
        /// `device_id` when both are sent: ids do not survive a replug or
        /// a PipeWire restart.
        device_node_name: Option<String>,
        /// Display name for the virtual mic node.
        mic_name: Option<String>,
        /// Voice-gate threshold.
        threshold: Option<i32>,
        /// High-pass rumble filter on/off.
        rumble_filter: Option<bool>,
        /// Auto-level stage on/off.
        auto_level: Option<bool>,
    },
    /// Tears the virtual mic down.
    Disable,
    /// Asks for a full [`DaemonState`] snapshot.
    GetState,
    /// Stops the daemon process itself.
    Shutdown,
    /// Starts capturing the voice test (raw and processed side by side).
    StartRecording,
    /// Finalizes the voice-test recording files.
    StopRecording,
    /// Plays back the raw voice-test capture.
    PlayOriginal,
    /// Stops raw-capture playback.
    StopOriginal,
    /// Plays back the processed voice-test capture.
    PlayFiltered,
    /// Stops processed-capture playback.
    StopFiltered,
}

/// Bump whenever the wire format changes meaning. serde fills missing
/// `Option` fields with `None` even without `#[serde(default)]`, so adding
/// an optional field never makes a new client reject an old daemon's state;
/// this required field is the explicit trip-wire that sends clients down
/// their restart-and-upgrade path.
pub const PROTOCOL_VERSION: u32 = 2;

/// The daemon's full observable state; the GUI reconciles its controls and
/// status display from this snapshot on every poll.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DaemonState {
    /// Must equal [`PROTOCOL_VERSION`]; absent (pre-0.4 daemons) or
    /// different means the daemon predates this client.
    pub protocol_version: u32,
    /// Current lifecycle state of the virtual mic.
    pub status: Status,
    /// PipeWire id of the configured capture device, if any.
    pub device_id: Option<u32>,
    /// The configured device's durable `node.name`.
    pub device_node_name: Option<String>,
    /// Display name of the virtual mic node.
    pub mic_name: String,
    /// Voice-gate threshold.
    pub threshold: i32,
    /// Defaulted so a pre-0.5 daemon's payload still parses and reaches the
    /// version check above instead of dying as a parse error; likewise for
    /// `auto_level`.
    #[serde(default)]
    pub rumble_filter: bool,
    /// Auto-level stage on/off; defaulted for the same reason.
    #[serde(default)]
    pub auto_level: bool,
    /// Whether the mic is up but running on silence while it waits for its
    /// capture device to return. Defaulted so a pre-1.0 daemon's payload
    /// still parses; an older client that does not read it simply shows the
    /// mic as active, which it is.
    #[serde(default)]
    pub capture_detached: bool,
    /// Whether the raw voice-test capture is currently playing.
    pub is_playing_original: bool,
    /// Whether the processed voice-test capture is currently playing.
    pub is_playing_filtered: bool,
    /// Why the mic is down, if it is; cleared on a successful
    /// enable/disable. Overloaded for recorder/player failures too: a
    /// crashed `NativeRecorder` or `NativePlayer` surfaces its reason here
    /// on the next `GetState` poll (those subsystems are not represented
    /// elsewhere in this struct). Stretches the field's "why the mic is
    /// down" contract intentionally, and persists until the next
    /// enable/disable.
    pub last_error: Option<String>,
}

/// The daemon's single reply to a [`DaemonCommand`].
#[derive(Debug, Serialize, Deserialize)]
pub enum DaemonResponse {
    /// The command succeeded with nothing to return.
    Ok,
    /// The snapshot answering [`DaemonCommand::GetState`].
    State(DaemonState),
    /// The command failed; a human-readable reason.
    Error(String),
}

/// The directory holding the daemon's socket, pid file and log.
///
/// The XDG base-directory spec treats an unset *or empty* `XDG_RUNTIME_DIR`
/// as absent, and a relative value is worse than absent: it would put the
/// control socket in whatever directory the process happens to run from,
/// outside the 0700 runtime dir and reachable by every other local user.
/// One `is_absolute` check rejects all three.
pub fn runtime_dir() -> Result<PathBuf, std::io::Error> {
    runtime_dir_from(std::env::var("XDG_RUNTIME_DIR").ok().as_deref()).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "XDG_RUNTIME_DIR is not set to an absolute path; spectral needs it \
             for the daemon socket, pid file and log",
        )
    })
}

/// The pure half of [`runtime_dir`]. Split out so the accept/reject rule is
/// testable: reading it back through the environment would mean mutating
/// process-global state that the parallel test harness shares.
fn runtime_dir_from(value: Option<&str>) -> Option<PathBuf> {
    let path = PathBuf::from(value?);
    path.is_absolute().then_some(path)
}

fn get_socket_path() -> Result<PathBuf, std::io::Error> {
    Ok(runtime_dir()?.join("spectral-daemon.sock"))
}

fn get_pid_file_path() -> Result<PathBuf, std::io::Error> {
    Ok(runtime_dir()?.join("spectral-daemon.pid"))
}

/// Where a spawned daemon's stdout and stderr go, beside the socket and pid
/// file. There is deliberately no second location: a daemon that cannot
/// resolve the runtime dir cannot bind its socket either, so a log written
/// elsewhere would only describe a session that was already lost — and a
/// predictable `/tmp` path opened with `truncate` is a symlink target on a
/// shared system. Failing here is what makes the client report the missing
/// runtime dir rather than a spawn error downstream of it.
///
/// Clients open this append-only and only the winning daemon truncates it,
/// so two clients racing to spawn cannot clobber each other's writes or
/// strand one of them at a stale offset.
pub fn daemon_log_path() -> Result<PathBuf, std::io::Error> {
    Ok(runtime_dir()?.join("spectral-daemon.log"))
}

/// Unblock a daemon sitting in [`DaemonServer::accept`] so its loop can
/// notice that its running flag has been cleared.
///
/// Connecting and dropping the stream is the whole trick: the accept
/// returns, the loop re-checks the flag, and the pointless connection is
/// never served. Best effort — if the socket is already gone the loop has
/// nothing left to wake from.
pub fn wake_accept_loop() {
    if let Ok(path) = get_socket_path() {
        let _ = UnixStream::connect(path);
    }
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

/// A client's end of the daemon socket: one blocking request/response
/// exchange at a time. The read buffer lives with the connection: a
/// per-call `BufReader` could read ahead past one line and silently drop
/// the buffered rest when it goes out of scope.
pub struct DaemonClient {
    reader: BufReader<UnixStream>,
}

impl DaemonClient {
    /// Connects to the daemon socket without retrying; see
    /// [`DaemonClient::connect_with_retry`] for the spawn-tolerant variant.
    pub fn connect() -> Result<Self, std::io::Error> {
        let socket_path = get_socket_path()?;
        let stream = UnixStream::connect(socket_path)?;
        stream.set_read_timeout(Some(CONNECTION_READ_TIMEOUT))?;
        Ok(Self {
            reader: BufReader::new(stream),
        })
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

    /// Sends one command and blocks until its response arrives.
    pub fn send_command(
        &mut self,
        command: DaemonCommand,
    ) -> Result<DaemonResponse, std::io::Error> {
        let json = serde_json::to_string(&command)?;
        writeln!(self.reader.get_mut(), "{json}")?;
        self.reader.get_mut().flush()?;

        let mut response_line = String::new();
        self.reader.read_line(&mut response_line)?;

        serde_json::from_str(&response_line)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }
}

/// The daemon's listening end; holds an exclusive lock on the pid file for
/// the daemon's lifetime, and dropping it removes the socket and pid files.
pub struct DaemonServer {
    listener: UnixListener,
    socket_path: PathBuf,
    pid_file_path: PathBuf,
    /// Held, never read: the OS lock on the pid file is what serializes
    /// daemon startup, and it lives exactly as long as this handle.
    _pid_lock: File,
}

impl DaemonServer {
    /// Claims the pid file and socket. An exclusive lock on the pid file —
    /// not the recorded pid's liveness — decides ownership: two daemons
    /// racing the old liveness check could interleave so the second
    /// unlinked the first's freshly bound socket, leaving an invisible
    /// zombie daemon. The kernel drops the lock on process exit, including
    /// the Shutdown path's `exit(0)` that skips `Drop`, so a crashed or
    /// shut-down daemon never blocks the next one.
    pub fn bind() -> Result<Self, std::io::Error> {
        let socket_path = get_socket_path()?;
        let pid_file_path = get_pid_file_path()?;

        let pid_file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&pid_file_path)?;
        match pid_file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                let holder = pid_file::read_pid(&pid_file_path)
                    .map(|pid| format!(" (PID {pid})"))
                    .unwrap_or_default();
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AddrInUse,
                    format!("Another daemon is already running{holder}"),
                ));
            }
            Err(TryLockError::Error(e)) => return Err(e),
        }
        pid_file::write_pid_to(&pid_file)?;

        // Under the held lock any existing socket file is stale by
        // definition; sweep it and bind fresh.
        match std::fs::remove_file(&socket_path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        // Left blocking on purpose. Polling accept on a timer costs every
        // client half the poll interval before its connection is even
        // picked up, and the GUI's heartbeat runs that wait on its
        // event-loop thread. `wake_accept_loop` is how shutdown gets the
        // loop moving again instead.
        let listener = UnixListener::bind(&socket_path)?;

        Ok(Self {
            listener,
            socket_path,
            pid_file_path,
            _pid_lock: pid_file,
        })
    }

    /// Waits for the next client. Blocks until one connects — including the
    /// throwaway connection [`wake_accept_loop`] makes to release a
    /// shutting-down daemon.
    pub fn accept(&self) -> Result<DaemonConnection, std::io::Error> {
        let (stream, _) = self.listener.accept()?;
        Ok(DaemonConnection {
            reader: BufReader::new(stream),
        })
    }
}

impl Drop for DaemonServer {
    fn drop(&mut self) {
        // Socket first, pid file last. The lock lives on the pid file's
        // inode, not its path, so unlinking that path opens the gate while
        // this daemon is still tearing down: a successor creates a fresh
        // inode, locks it uncontended, and binds its own socket — which the
        // socket removal here would then delete out from under it, leaving
        // a live daemon nothing can connect to. Releasing the gate last
        // keeps the successor parked on `try_lock` until we are done.
        let _ = std::fs::remove_file(&self.socket_path);
        pid_file::cleanup(&self.pid_file_path);
    }
}

/// One accepted client connection, served on its own handler thread. The
/// read buffer lives with the connection so a client that pipelines two
/// commands cannot lose the second to a discarded per-call `BufReader`.
pub struct DaemonConnection {
    reader: BufReader<UnixStream>,
}

impl DaemonConnection {
    /// Reads the next command line, timing out after
    /// [`CONNECTION_READ_TIMEOUT`] so a silent client cannot pin its
    /// handler thread forever.
    pub fn read_command(&mut self) -> Result<DaemonCommand, std::io::Error> {
        self.reader
            .get_ref()
            .set_read_timeout(Some(CONNECTION_READ_TIMEOUT))?;

        let mut line = String::new();
        self.reader.read_line(&mut line)?;

        if line.trim().is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "Connection closed",
            ));
        }

        serde_json::from_str(&line).map_err(|e| {
            eprintln!("[daemon] failed to parse command: {e}");
            std::io::Error::new(std::io::ErrorKind::InvalidData, e)
        })
    }

    /// Writes one JSON response line and flushes it.
    pub fn send_response(&mut self, response: DaemonResponse) -> Result<(), std::io::Error> {
        let json = serde_json::to_string(&response)?;
        writeln!(self.reader.get_mut(), "{json}")?;
        self.reader.get_mut().flush()
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
        assert!(!state.rumble_filter);
        assert!(!state.auto_level);
        assert!(!state.capture_detached);
    }

    #[test]
    fn missing_protocol_version_fails_to_parse() {
        assert!(serde_json::from_str::<DaemonState>(OLD_STATE).is_err());
    }

    /// The mirror of [`missing_option_fields_parse_as_none`] for the command
    /// direction. Commands carry no version field, so this tolerance *is*
    /// the compatibility story on that side, and it is what the bump policy
    /// on [`PROTOCOL_VERSION`] means by "adding an `Option` field is
    /// compatible" — the only test that exercises that claim for commands.
    #[test]
    fn a_pre_0_5_enable_parses_with_the_added_fields_as_none() {
        let old = r#"{"Enable":{"device_id":5,"mic_name":"My-Mic","threshold":70}}"#;
        let command: DaemonCommand =
            serde_json::from_str(old).expect("an old client's Enable must still parse");
        match command {
            DaemonCommand::Enable {
                device_id,
                device_node_name,
                mic_name,
                threshold,
                rumble_filter,
                auto_level,
            } => {
                assert_eq!(device_id, Some(5));
                assert_eq!(device_node_name, None, "a later version added this field");
                assert_eq!(mic_name.as_deref(), Some("My-Mic"));
                assert_eq!(threshold, Some(70));
                assert_eq!(rumble_filter, None, "0.5 added this field");
                assert_eq!(auto_level, None, "0.5 added this field");
            }
            other => panic!("expected Enable, got {other:?}"),
        }
    }

    // Serialize, deserialize, re-serialize. What byte-stability catches on
    // its own is an *asymmetric* representation — a field that serializes
    // but not back, a one-way rename — because a symmetric change moves both
    // directions together and still re-serializes to itself. Catching that
    // is the goldens' job, in the callers, not this helper's. The decoded
    // value comes back alongside the encoding so a caller can assert the
    // payload survived and not merely that the bytes were stable.
    //
    // The frame is one JSON value per line (`writeln!` out, `read_line`
    // back), so a literal newline anywhere in the encoding would split one
    // message into two and desynchronize the connection for good.
    fn round_trip<T>(value: &T) -> (T, String)
    where
        T: Serialize + for<'de> Deserialize<'de>,
    {
        let json = serde_json::to_string(value).expect("must serialize");
        assert!(
            !json.contains('\n'),
            "a raw newline in {json:?} would split the frame"
        );
        let back: T = serde_json::from_str(&json).expect("must deserialize");
        let again = serde_json::to_string(&back).expect("must re-serialize");
        assert_eq!(again, json, "wire form changed across the round trip");
        (back, json)
    }

    #[test]
    fn command_wire_forms_are_stable_round_trips() {
        let cases = [
            DaemonCommand::Enable {
                device_id: None,
                device_node_name: None,
                mic_name: None,
                threshold: None,
                rumble_filter: None,
                auto_level: None,
            },
            DaemonCommand::Enable {
                device_id: Some(5),
                device_node_name: Some("alsa_input.usb-Blue_Yeti-00.analog-stereo".to_string()),
                mic_name: Some("My-Mic".to_string()),
                threshold: Some(70),
                rumble_filter: Some(true),
                auto_level: Some(false),
            },
            DaemonCommand::Disable,
            DaemonCommand::GetState,
            DaemonCommand::Shutdown,
            DaemonCommand::StartRecording,
            DaemonCommand::StopRecording,
            DaemonCommand::PlayOriginal,
            DaemonCommand::StopOriginal,
            DaemonCommand::PlayFiltered,
            DaemonCommand::StopFiltered,
        ];
        for command in cases {
            assert_eq!(round_trip(&command).1, command_golden(&command));
        }
    }

    /// Keeping the goldens here rather than beside the cases means a new
    /// variant cannot be added without authoring its wire form: the match is
    /// exhaustive, and an arm has nothing to return but a `&str`.
    ///
    /// Each golden is compared against what serde actually produced, by the
    /// round-trip loop in the test above, so a drifted literal or wire form
    /// fails loudly. What this helper cannot do is force coverage: an arm whose
    /// variant never joins `cases` is never compared at all. Extending
    /// `cases` is part of adding the variant.
    ///
    /// `Enable` is split by shape because the table exercises it twice.
    fn command_golden(command: &DaemonCommand) -> &'static str {
        match command {
            DaemonCommand::Enable {
                device_id: None,
                device_node_name: None,
                mic_name: None,
                threshold: None,
                rumble_filter: None,
                auto_level: None,
            } => {
                r#"{"Enable":{"device_id":null,"device_node_name":null,"mic_name":null,"threshold":null,"rumble_filter":null,"auto_level":null}}"#
            }
            DaemonCommand::Enable { .. } => {
                r#"{"Enable":{"device_id":5,"device_node_name":"alsa_input.usb-Blue_Yeti-00.analog-stereo","mic_name":"My-Mic","threshold":70,"rumble_filter":true,"auto_level":false}}"#
            }
            DaemonCommand::Disable => r#""Disable""#,
            DaemonCommand::GetState => r#""GetState""#,
            DaemonCommand::Shutdown => r#""Shutdown""#,
            DaemonCommand::StartRecording => r#""StartRecording""#,
            DaemonCommand::StopRecording => r#""StopRecording""#,
            DaemonCommand::PlayOriginal => r#""PlayOriginal""#,
            DaemonCommand::StopOriginal => r#""StopOriginal""#,
            DaemonCommand::PlayFiltered => r#""PlayFiltered""#,
            DaemonCommand::StopFiltered => r#""StopFiltered""#,
        }
    }

    #[test]
    fn response_wire_forms_are_stable_round_trips() {
        // The golden pins protocol_version at 2 as a literal: bumping the
        // constant means deliberately editing this string.
        let full_state = DaemonState {
            protocol_version: PROTOCOL_VERSION,
            status: Status::Active,
            device_id: Some(42),
            device_node_name: Some("alsa_input.usb-Blue_Yeti-00.analog-stereo".to_string()),
            mic_name: "Spectral-Mic".to_string(),
            threshold: 80,
            rumble_filter: true,
            auto_level: false,
            capture_detached: false,
            is_playing_original: false,
            is_playing_filtered: true,
            last_error: Some("pipeline died".to_string()),
        };
        // A fresh daemon's snapshot, pinned alongside the populated one
        // because every `Option` here is `None`: a `skip_serializing_if`
        // added to any of them would leave the golden above untouched
        // while quietly dropping three keys from the reply clients see
        // most often.
        let idle_state = DaemonState {
            protocol_version: PROTOCOL_VERSION,
            status: Status::Inactive,
            device_id: None,
            device_node_name: None,
            mic_name: "Spectral-Mic".to_string(),
            threshold: 50,
            rumble_filter: false,
            auto_level: false,
            capture_detached: false,
            is_playing_original: false,
            is_playing_filtered: false,
            last_error: None,
        };
        // The third status. It reaches clients on every enable and disable, so
        // its spelling needs a golden of its own; without one, renaming it
        // would be a wire break the suite passes.
        let pending_state = DaemonState {
            protocol_version: PROTOCOL_VERSION,
            status: Status::Pending,
            device_id: Some(42),
            device_node_name: Some("alsa_input.usb-Blue_Yeti-00.analog-stereo".to_string()),
            mic_name: "Spectral-Mic".to_string(),
            threshold: 80,
            rumble_filter: true,
            auto_level: false,
            capture_detached: false,
            is_playing_original: false,
            is_playing_filtered: false,
            last_error: None,
        };
        let cases = [
            DaemonResponse::Ok,
            DaemonResponse::State(full_state),
            DaemonResponse::State(idle_state),
            DaemonResponse::State(pending_state),
            DaemonResponse::Error("no such device".to_string()),
        ];
        for response in cases {
            assert_eq!(round_trip(&response).1, response_golden(&response));
        }
    }

    /// Coverage guard for the response table; see [`command_golden`] for what
    /// that does and does not buy. Two separate things guard [`Status`], which
    /// crosses the wire as a bare string. Matching on `status` instead of
    /// through a wildcard makes these arms exhaustive over it, so a fourth
    /// variant fails to compile rather than borrowing a neighbor's golden;
    /// the spelling of the three that exist is pinned by having a golden
    /// each, which is what a `serde` rename would break. `DaemonState`'s
    /// *fields* need neither — a new one lands in the JSON and breaks all
    /// three goldens on its own.
    fn response_golden(response: &DaemonResponse) -> &'static str {
        match response {
            DaemonResponse::Ok => r#""Ok""#,
            DaemonResponse::State(DaemonState {
                status: Status::Active,
                ..
            }) => {
                r#"{"State":{"protocol_version":2,"status":"Active","device_id":42,"device_node_name":"alsa_input.usb-Blue_Yeti-00.analog-stereo","mic_name":"Spectral-Mic","threshold":80,"rumble_filter":true,"auto_level":false,"capture_detached":false,"is_playing_original":false,"is_playing_filtered":true,"last_error":"pipeline died"}}"#
            }
            DaemonResponse::State(DaemonState {
                status: Status::Pending,
                ..
            }) => {
                r#"{"State":{"protocol_version":2,"status":"Pending","device_id":42,"device_node_name":"alsa_input.usb-Blue_Yeti-00.analog-stereo","mic_name":"Spectral-Mic","threshold":80,"rumble_filter":true,"auto_level":false,"capture_detached":false,"is_playing_original":false,"is_playing_filtered":false,"last_error":null}}"#
            }
            DaemonResponse::State(DaemonState {
                status: Status::Inactive,
                ..
            }) => {
                r#"{"State":{"protocol_version":2,"status":"Inactive","device_id":null,"device_node_name":null,"mic_name":"Spectral-Mic","threshold":50,"rumble_filter":false,"auto_level":false,"capture_detached":false,"is_playing_original":false,"is_playing_filtered":false,"last_error":null}}"#
            }
            DaemonResponse::Error(_) => r#"{"Error":"no such device"}"#,
        }
    }

    #[test]
    fn unicode_and_escapes_survive_the_round_trip() {
        // Validation lives at the edges; the wire itself is
        // representation-only, so non-ASCII names and error text with
        // quotes, backslashes and newlines must arrive untouched.
        let name = "caf\u{e9} \u{1f3a4}";
        let (back, _) = round_trip(&DaemonCommand::Enable {
            device_id: None,
            device_node_name: None,
            mic_name: Some(name.to_string()),
            threshold: None,
            rumble_filter: None,
            auto_level: None,
        });
        match back {
            DaemonCommand::Enable { mic_name, .. } => assert_eq!(mic_name.as_deref(), Some(name)),
            other => panic!("the round trip changed the variant: {other:?}"),
        }

        let reason = "quote \" backslash \\ newline\n";
        let (back, _) = round_trip(&DaemonResponse::Error(reason.to_string()));
        match back {
            DaemonResponse::Error(text) => assert_eq!(text, reason),
            other => panic!("the round trip changed the variant: {other:?}"),
        }
    }

    /// Only an absolute path may name the runtime dir. An empty value is
    /// absent per the XDG spec, and a relative one would resolve the socket
    /// and pid file against the process's cwd — world-reachable, unlike the
    /// 0700 runtime dir they belong in.
    #[test]
    fn only_an_absolute_runtime_dir_is_accepted() {
        for value in [None, Some(""), Some("relative/dir"), Some(" "), Some("~")] {
            assert_eq!(
                runtime_dir_from(value),
                None,
                "{value:?} must not resolve to a runtime dir"
            );
        }

        assert_eq!(
            runtime_dir_from(Some("/run/user/1000")),
            Some(PathBuf::from("/run/user/1000")),
            "an absolute path is the one accepted form"
        );
    }
}
