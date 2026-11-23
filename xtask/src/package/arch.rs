//! The Arch package: `makepkg` in an `archlinux:base-devel` container from
//! a `git archive` tarball of the commit, then `namcap`.

use std::fs;
use std::process::Command;

use super::{Build, Report, cmd, podman_script};
use crate::workspace;

const BUILD_SCRIPT: &str = include_str!("../../scripts/arch-build.sh");

/// Builds the package in a container, lints it, and install-checks it.
pub fn run(build: &Build, report: &mut Report) -> Result<(), String> {
    let work = build.out.join("work");
    let pkg = build.out.join("pkg");
    for dir in [&work, &pkg] {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let pkgbuild = build.src.join("packaging/arch/PKGBUILD");
    fs::copy(&pkgbuild, work.join("PKGBUILD"))
        .map_err(|e| format!("{}: {e}", pkgbuild.display()))?;
    let name = format!("spectral-mic-{}", build.version);
    cmd::run(
        Command::new("git")
            .arg("-C")
            .arg(workspace::root())
            .args(["archive", "--format=tar.gz"])
            .arg(format!("--prefix={name}/"))
            .arg("--output")
            .arg(work.join(format!("{name}.tar.gz")))
            .arg(&build.commit),
    )?;
    cmd::run(&mut podman_script(
        "docker.io/library/archlinux:base-devel",
        &[(&work, "/in:ro"), (&pkg, "/out")],
        BUILD_SCRIPT,
    ))?;
    report.pass("build");
    report.note("namcap: findings printed above, to triage by hand");
    super::check::run(
        build,
        &super::check::Target {
            name: "arch",
            base_image: "docker.io/library/archlinux:base",
            install_script: "pacman -Syu --noconfirm && pacman -U --noconfirm /pkg/*.pkg.tar.zst",
            command: &["spectral"],
            podman_options: &[],
        },
        report,
    )
}

#[cfg(test)]
mod tests {
    use std::fs;

    use crate::workspace;

    fn pkgbuild_version(pkgbuild: &str) -> Option<&str> {
        pkgbuild
            .lines()
            .find_map(|line| line.strip_prefix("pkgver="))
    }

    /// The PKGBUILD's `pkgver` is one of the spots
    /// `packaging/bump-version.sh` moves; this fails the release if it ever
    /// stops moving with the rest.
    #[test]
    fn the_pkgbuild_version_matches_the_crate() {
        let root = workspace::root();
        let pkgbuild = fs::read_to_string(root.join("packaging/arch/PKGBUILD")).unwrap();
        let version = workspace::crate_version(&root).unwrap();
        assert_eq!(pkgbuild_version(&pkgbuild), Some(version.as_str()));
    }
}
