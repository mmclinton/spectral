//! Platform-specific implementations
//!
//! Spectral is Linux-only today. This seam exists so OS integration lives
//! behind one module (audio device discovery, the processing pipeline, IPC,
//! and state persistence), leaving room for other platforms later.
//!
//! The re-exports below are the seam's whole public surface: everything the
//! binaries may name is `platform::<module>`, and the `linux` module itself
//! stays crate-private so there is exactly one path to any given item. A
//! second platform re-points these names without touching a call site.
//!
//! A module belongs in the list only if a binary actually uses it. The
//! PipeWire pipeline and the DSP stages are internals of the Linux
//! implementation and deliberately absent.

#[cfg(target_os = "linux")]
pub(crate) mod linux;

#[cfg(target_os = "linux")]
pub use linux::audio_devices;
#[cfg(target_os = "linux")]
pub use linux::audio_processor;
#[cfg(target_os = "linux")]
pub use linux::ipc;
#[cfg(target_os = "linux")]
pub use linux::persistence;
#[cfg(target_os = "linux")]
pub use linux::recorder;
