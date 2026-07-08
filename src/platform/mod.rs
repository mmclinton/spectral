//! Platform-specific implementations
//!
//! Spectral is Linux-only. This seam exists so OS integration lives behind
//! one module (audio device discovery, the processing pipeline, IPC, and
//! state persistence), leaving room for other platforms later.

#[cfg(target_os = "linux")]
pub mod linux;

#[cfg(target_os = "linux")]
pub use linux::audio_devices;
#[cfg(target_os = "linux")]
pub use linux::audio_processor;
#[cfg(target_os = "linux")]
pub use linux::ipc;
#[cfg(target_os = "linux")]
pub use linux::persistence;
#[cfg(target_os = "linux")]
pub use linux::pipewire;
