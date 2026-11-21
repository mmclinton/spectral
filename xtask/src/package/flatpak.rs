//! The Flatpak, built on the host with the installed `org.flatpak.Builder`
//! into a local repository, linted, bundled, and asked for its version.
//! Its dependencies are the fixed runtime, so it has no install check.

use std::fs;
use std::path::Path;
use std::process::Command;

use super::{Build, Report, cmd};
use crate::install::APP_ID;

/// Lets `flatpak install` of the bundle fetch the runtime it names.
const RUNTIME_REPO: &str = "https://dl.flathub.org/repo/flathub.flatpakrepo";

fn builder() -> Command {
    let mut command = Command::new("flatpak");
    command.args(["run", "org.flatpak.Builder"]);
    command
}

/// Builds, lints and bundles the Flatpak, then asks it for its version.
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

    let step = "spectral --version";
    let expected = format!("spectral {}", build.version);
    // Judged on the output alone: `flatpak-builder --run` exits 1 even when
    // the command it runs succeeds (`--run … true` does too).
    match cmd::capture(
        builder()
            .arg("--run")
            .arg(&state_dir)
            .arg(&builddir)
            .arg(&manifest)
            .args(["spectral", "--version"]),
    ) {
        Ok((_, text)) if text.lines().any(|line| line.trim() == expected) => {
            report.pass(step);
        }
        Ok((_, text)) => report.fail(step, format!("expected `{expected}`, got: {}", text.trim())),
        Err(e) => report.fail(step, e),
    }
    Ok(())
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
