//! The deb: debhelper in an `ubuntu:24.04` container on the export, with
//! `debian/` copied in from `packaging/deb/` and its changelog generated
//! here, then `lintian`.

use std::fs;
use std::process::Command;

use super::check::{self, Target};
use super::{Build, Report, cmd, podman_script};
use crate::workspace;

const BUILD_SCRIPT: &str = include_str!("../../scripts/deb-build.sh");

/// Builds the package in a container, lints it, and install-checks it.
pub fn run(build: &Build, report: &mut Report) -> Result<(), String> {
    let debian = build.src.join("debian");
    cmd::run(
        Command::new("cp")
            .arg("-a")
            .arg(build.src.join("packaging/deb/debian"))
            .arg(&debian),
    )?;
    let date = cmd::output(
        Command::new("git")
            .env("LC_ALL", "C")
            .arg("-C")
            .arg(workspace::root())
            .args([
                "log",
                "-1",
                "--format=%ad",
                "--date=format:%a, %d %b %Y %H:%M:%S %z",
            ])
            .arg(&build.commit),
    )?;
    let path = debian.join("changelog");
    fs::write(&path, changelog(&build.version, date.trim()))
        .map_err(|e| format!("{}: {e}", path.display()))?;

    let pkg = build.out.join("pkg");
    fs::create_dir_all(&pkg).map_err(|e| format!("{}: {e}", pkg.display()))?;
    cmd::run(&mut podman_script(
        "docker.io/library/ubuntu:24.04",
        &[(&build.src, "/in:ro"), (&pkg, "/out")],
        BUILD_SCRIPT,
    ))?;
    report.pass("build");
    report.note("lintian: findings printed above, to triage by hand");

    check::run(
        build,
        &Target {
            name: "deb",
            base_image: "docker.io/library/ubuntu:24.04",
            // --no-install-recommends: the check is that the declared
            // dependencies alone are enough.
            install_script: "apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends /pkg/*.deb",
            command: &["spectral"],
            podman_options: &[],
        },
        report,
    )
}

/// `debian/changelog` for `version`, dated `date` in RFC 2822 form, for
/// Ubuntu 24.04 (noble), the release it is built on. It is generated so a
/// release bump never has to touch the deb, and dated with the commit so
/// the same commit builds the same package. The revision is always 1: a
/// fix to the packaging alone ships as a new patch release, so every
/// published package is exactly one tag.
fn changelog(version: &str, date: &str) -> String {
    format!(
        "spectral-mic ({version}-1) noble; urgency=medium\n\n  * Spectral {version}; CHANGELOG.md in the source lists the changes.\n\n -- mmc <github.mmc@pm.me>  {date}\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_changelog_names_the_version_with_the_first_revision() {
        assert_eq!(
            changelog("0.11.0", "Wed, 04 Nov 2026 10:00:00 -0500"),
            "spectral-mic (0.11.0-1) noble; urgency=medium\n\n  * Spectral 0.11.0; CHANGELOG.md in the source lists the changes.\n\n -- mmc <github.mmc@pm.me>  Wed, 04 Nov 2026 10:00:00 -0500\n"
        );
    }
}
