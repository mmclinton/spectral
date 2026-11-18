//! Facts about the Spectral source tree that more than one task needs.

use std::path::{Path, PathBuf};

/// The workspace root, one level above xtask's own manifest.
pub fn root() -> PathBuf {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .map_or_else(|| manifest_dir.to_path_buf(), Path::to_path_buf)
}

/// Cargo's target directory: `$CARGO_TARGET_DIR` when set (a relative one
/// is relative to the working directory, as Cargo reads it), otherwise
/// `target/` in the root.
pub fn target_dir() -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR").map_or_else(|| root().join("target"), PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_root_holds_the_workspace_manifest_and_xtask() {
        assert!(root().join("Cargo.toml").is_file());
        assert!(root().join("xtask").is_dir());
    }
}
