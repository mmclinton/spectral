pub mod platform;
pub mod state;

// Re-export platform modules for backward compatibility and convenience
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
#[cfg(target_os = "linux")]
pub use platform::linux::pulse;
