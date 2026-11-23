//! The Flatpak, built on the host with the installed `org.flatpak.Builder`
//! into a local repository, linted and bundled. The bundle then gets the
//! install check every format gets, in a container standing in for the
//! user's system, with the runtime fetched from Flathub as a user's install
//! fetches it.

use std::fs;
use std::path::Path;
use std::process::Command;

use super::{Build, Report, check, cmd};
use crate::install::APP_ID;

/// Lets `flatpak install` of the bundle fetch the runtime it names.
const RUNTIME_REPO: &str = "https://dl.flathub.org/repo/flathub.flatpakrepo";

const HOST_IMAGE: &str = "localhost/spectral-xtask-flatpak-host";
/// The user's side of the sandbox: Flatpak, and the PipeWire and WirePlumber
/// the sandboxed app talks to.
const HOST_CONTAINERFILE: &str = "FROM docker.io/library/ubuntu:24.04
RUN apt-get update \\
 && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends flatpak pipewire wireplumber dbus-user-session ca-certificates \\
 && rm -rf /var/lib/apt/lists/*
";

fn builder() -> Command {
    let mut command = Command::new("flatpak");
    command.args(["run", "org.flatpak.Builder"]);
    command
}

/// Builds, lints and bundles the Flatpak, then install-checks the bundle.
pub fn run(build: &Build, report: &mut Report) -> Result<(), String> {
    let manifest = build.src.join(format!("packaging/flatpak/{APP_ID}.yaml"));
    let builddir = build.out.join("build");
    let repo = build.out.join("repo");
    // Beside the format's directory, so the downloaded crates outlive the
    // fresh start every run makes; the build itself stays clean through
    // --force-clean. Every flatpak-builder call names it, or the tool falls
    // back to `.flatpak-builder` in the working directory.
    let state_dir = format!(
        "--state-dir={}",
        build.out.with_file_name("flatpak-state").display()
    );
    cmd::run(
        builder()
            .args(["--user", "--install-deps-from=flathub", "--force-clean"])
            .arg(&state_dir)
            .arg(format!("--repo={}", repo.display()))
            .arg(&builddir)
            .arg(&manifest),
    )?;
    report.pass("build");

    lint("manifest", &manifest);
    lint("repo", &repo);
    report.note("flatpak-builder-lint: findings printed above, to triage by hand");

    let pkg = build.out.join("pkg");
    fs::create_dir_all(&pkg).map_err(|e| format!("{}: {e}", pkg.display()))?;
    cmd::run(
        Command::new("flatpak")
            .arg("build-bundle")
            .arg(format!("--runtime-repo={RUNTIME_REPO}"))
            .arg(&repo)
            .arg(pkg.join(format!("{APP_ID}.flatpak")))
            .arg(APP_ID),
    )?;
    report.pass("bundle");

    check::build_image(build, HOST_IMAGE, HOST_CONTAINERFILE)?;
    check::run(
        build,
        &check::Target {
            name: "flatpak",
            base_image: HOST_IMAGE,
            // System-wide, so the desktop user the later steps run as can
            // run what root installed; the system bus is for Flatpak's
            // parental-controls check. The bundle names Flathub as its
            // runtime's repository, so the install adds it as a user's
            // does. --no-related leaves out the GL extension, which the
            // software renderer never loads. The install's triggers only
            // refresh desktop caches, and cannot run bubblewrap as root.
            install_script: "mkdir -p /run/dbus && dbus-daemon --system --fork && flatpak install --system -y --noninteractive --no-related /pkg/*.flatpak",
            command: &["flatpak", "run", APP_ID],
            // Lets bubblewrap mount /proc inside the container.
            podman_options: &["--security-opt", "unmask=ALL"],
        },
        report,
    )
}

/// Prints `flatpak-builder-lint`'s findings; they are triaged by hand, not
/// a reason to stop.
fn lint(kind: &str, path: &Path) {
    let mut command = Command::new("flatpak");
    command
        .args([
            "run",
            "--command=flatpak-builder-lint",
            "org.flatpak.Builder",
            kind,
        ])
        .arg(path);
    match cmd::capture(&mut command) {
        Ok((_, text)) => eprintln!("flatpak-builder-lint {kind}:\n{text}"),
        Err(e) => eprintln!("warning: {e}"),
    }
}
