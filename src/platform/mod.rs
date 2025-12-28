//! Platform-specific implementations
//!
//! This module provides platform abstractions for Spectral.
//! Each platform (Linux, macOS, Windows) has its own submodule
//! with implementations for:
//! - Audio device discovery
//! - Audio processing pipeline
//! - Inter-process communication
//! - State persistence
//!
//! The appropriate platform module is selected at compile time
//! using conditional compilation.

#[cfg(target_os = "linux")]
pub mod linux;

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(target_os = "windows")]
pub mod windows;

// Re-export the current platform's modules for convenient access
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
#[cfg(target_os = "linux")]
pub use linux::pulse;
