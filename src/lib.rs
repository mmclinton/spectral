//! Shared library for the two Spectral binaries: the `spectral` GUI control
//! panel and the `spectral-daemon` backend. Holds everything both sides need
//! to agree on — daemon state, the IPC wire format, settings persistence, and
//! the PipeWire audio integration. Not published as a standalone API.
//!
//! OS integration is reached through the [`platform`] seam — `platform::ipc`,
//! `platform::persistence`, and so on. Nothing is re-exported at the crate
//! root: a Linux module named there would have to move the day a second
//! platform lands.

#![warn(missing_docs)]

pub mod platform;
pub mod state;
