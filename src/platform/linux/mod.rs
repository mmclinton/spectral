//! Linux platform implementation
//!
//! This module contains all Linux-specific code for Spectral:
//! - PipeWire audio pipeline integration
//! - Audio device discovery via PipeWire
//! - Unix domain socket IPC
//! - XDG-compliant persistence

pub mod audio_devices;
pub mod audio_processor;
pub mod ipc;
pub mod persistence;
pub mod pipewire;
pub mod recorder;
pub mod stages;
