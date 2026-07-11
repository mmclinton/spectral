//! Shared library for the two Spectral binaries: the `spectral` GUI control
//! panel and the `spectral-daemon` backend. Holds everything both sides need
//! to agree on — daemon state, the IPC wire format, settings persistence, and
//! the PipeWire audio integration. Not published as a standalone API.

#![warn(missing_docs)]

pub mod platform;
pub mod state;

#[cfg(target_os = "linux")]
pub use platform::linux::audio_devices;
#[cfg(target_os = "linux")]
pub use platform::linux::audio_processor;
#[cfg(target_os = "linux")]
pub use platform::linux::ipc;
#[cfg(target_os = "linux")]
pub use platform::linux::persistence;
#[cfg(target_os = "linux")]
pub use platform::linux::pipewire;
