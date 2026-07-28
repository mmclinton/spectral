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

use ::pipewire::properties::{PropertiesBox, properties};

/// Client properties declaring this PipeWire connection a graph manager.
///
/// Spectral creates its own nodes, links them explicitly, and writes the
/// `default` metadata object. Under a Flatpak sandbox WirePlumber grants an
/// undeclared client only `rx` — enough to observe the graph, never to change
/// it — and reserves full permissions for clients that identify themselves
/// this way. Outside a sandbox the client already has full permissions, so
/// this changes nothing.
///
/// Reserved for the two connections that genuinely modify the graph; the
/// read-only registry queries and the voice-test streams stay unprivileged.
pub fn manager_client_properties() -> PropertiesBox {
    properties! {
        *::pipewire::keys::MEDIA_CATEGORY => "Manager",
    }
}
