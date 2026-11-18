//! Facts about the Spectral source tree that more than one task needs.

use std::fs;
use std::path::{Path, PathBuf};

/// The workspace root, one level above xtask's own manifest.
pub fn root() -> PathBuf {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .map_or_else(|| manifest_dir.to_path_buf(), Path::to_path_buf)
}

/// Cargo's target directory, wherever this build put it. xtask runs as
/// `<target-dir>[/<triple>]/<profile>/xtask`, so two levels up honors
/// `CARGO_TARGET_DIR`, `build.target-dir` and `build.target`, which apply
/// to the release build of Spectral as well. A `--target` flag given only
/// to that build does not reach xtask, which is what `install --binaries`
/// is for. (A test binary sits one level deeper, in `deps/`; no test calls
/// this.)
pub fn target_dir() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot locate xtask itself: {e}"))?;
    exe.parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .ok_or_else(|| format!("{} is not inside a Cargo target directory", exe.display()))
}

/// The `[package]` version in `tree`'s `Cargo.toml`.
pub fn crate_version(tree: &Path) -> Result<String, String> {
    let path = tree.join("Cargo.toml");
    let manifest = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    package_version(&manifest)
        .ok_or_else(|| format!("{}: no version under [package]", path.display()))
}

fn package_version(manifest: &str) -> Option<String> {
    let mut in_package = false;
    for line in manifest.lines().map(str::trim) {
        if line.starts_with('[') {
            in_package = line == "[package]";
        } else if in_package && let Some(quoted) = line.strip_prefix("version = \"") {
            return quoted.strip_suffix('"').map(str::to_string);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_package_version_is_read_from_its_own_section() {
        let manifest = "[workspace.package]\nedition = \"2024\"\n\n[package]\nname = \"spectral\"\nversion = \"1.2.3\"\n\n[dependencies]\nserde = { version = \"1.0\" }\n";
        assert_eq!(package_version(manifest), Some("1.2.3".to_string()));
    }

    #[test]
    fn a_version_outside_the_package_section_is_ignored() {
        assert_eq!(
            package_version("[dependencies]\nversion = \"9.9.9\"\n"),
            None
        );
    }

    #[test]
    fn the_crate_version_is_read_from_the_root_manifest() {
        let version = crate_version(&root()).unwrap();
        assert_eq!(version.split('.').count(), 3, "not X.Y.Z: {version}");
    }

    #[test]
    fn the_root_holds_the_workspace_manifest_and_xtask() {
        assert!(root().join("Cargo.toml").is_file());
        assert!(root().join("xtask").is_dir());
    }
}
