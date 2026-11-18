//! `cargo xtask package`: builds one format's package from a clean export
//! of a git ref and checks it. Nothing is signed or published; the
//! unsigned package is left in `target/packages/<format>/pkg/`.

mod arch;
mod check;
mod cmd;
mod deb;
mod flatpak;

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::workspace;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Flatpak,
    Arch,
    Deb,
}

impl Format {
    fn name(self) -> &'static str {
        match self {
            Self::Flatpak => "flatpak",
            Self::Arch => "arch",
            Self::Deb => "deb",
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Options {
    format: Format,
    git_ref: String,
}

impl Options {
    pub fn parse(args: &[String]) -> Result<Self, String> {
        let mut args = args.iter();
        let format = match args.next().map(String::as_str) {
            Some("flatpak") => Format::Flatpak,
            Some("arch") => Format::Arch,
            Some("deb") => Format::Deb,
            Some(other) => return Err(format!("package: unknown format '{other}'")),
            None => return Err("package: name a format".to_string()),
        };
        let git_ref = match (args.next().map(String::as_str), args.next()) {
            (None, _) => "HEAD".to_string(),
            (Some("--ref"), Some(value)) => value.clone(),
            (Some("--ref"), None) => return Err("package: --ref needs a git ref".to_string()),
            (Some(other), _) => return Err(format!("package: unknown argument '{other}'")),
        };
        if let Some(extra) = args.next() {
            return Err(format!("package: unexpected argument '{extra}'"));
        }
        Ok(Self { format, git_ref })
    }
}

/// What a format's build works from.
pub struct Build {
    /// The commit the ref resolved to.
    commit: String,
    /// A clean export of that commit.
    src: PathBuf,
    /// The format's working directory; the package lands in `pkg/` inside it.
    out: PathBuf,
    /// The crate version at that commit.
    version: String,
}

#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Pass,
    Note,
    Fail(String),
}

/// One line per step, printed at the end so the verdict is not lost in
/// the tools' own output.
#[derive(Default)]
pub struct Report {
    steps: Vec<(String, Outcome)>,
}

impl Report {
    pub fn pass(&mut self, step: impl Into<String>) {
        self.steps.push((step.into(), Outcome::Pass));
    }

    pub fn note(&mut self, step: impl Into<String>) {
        self.steps.push((step.into(), Outcome::Note));
    }

    pub fn fail(&mut self, step: impl Into<String>, why: impl Into<String>) {
        self.steps.push((step.into(), Outcome::Fail(why.into())));
    }

    fn print(&self) {
        for (step, outcome) in &self.steps {
            match outcome {
                Outcome::Pass => eprintln!("pass  {step}"),
                Outcome::Note => eprintln!("note  {step}"),
                Outcome::Fail(why) => eprintln!("FAIL  {step}: {why}"),
            }
        }
    }

    fn verdict(&self) -> Result<(), String> {
        let failed = self
            .steps
            .iter()
            .filter(|(_, outcome)| matches!(outcome, Outcome::Fail(_)))
            .count();
        if failed == 0 {
            Ok(())
        } else {
            Err(format!("package: {failed} check(s) failed"))
        }
    }
}

pub fn run(options: &Options) -> Result<(), String> {
    let root = workspace::root();
    let commit = cmd::output(
        Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["rev-parse", "--verify"])
            .arg(format!("{}^{{commit}}", options.git_ref)),
    )?
    .trim()
    .to_string();
    let out = workspace::target_dir()?
        .join("packages")
        .join(options.format.name());
    fresh_dir(&out)?;
    let src = out.join("src");
    export(&root, &commit, &src)?;
    let build = Build {
        version: workspace::crate_version(&src)?,
        commit,
        src,
        out,
    };
    eprintln!(
        "package: {} {} from {}",
        options.format.name(),
        build.version,
        build.commit
    );
    let mut report = Report::default();
    let result = match options.format {
        Format::Flatpak => flatpak::run(&build, &mut report),
        Format::Arch => arch::run(&build, &mut report),
        Format::Deb => deb::run(&build, &mut report),
    };
    report.print();
    result?;
    report.verdict()
}

/// Removes `dir` if it exists and creates it empty.
fn fresh_dir(dir: &Path) -> Result<(), String> {
    if let Err(e) = fs::remove_dir_all(dir)
        && e.kind() != io::ErrorKind::NotFound
    {
        return Err(format!("{}: {e}", dir.display()));
    }
    fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))
}

/// `git archive` of `commit` unpacked into `dest`: exactly the tracked
/// files, nothing from the working copy.
fn export(root: &Path, commit: &str, dest: &Path) -> Result<(), String> {
    fs::create_dir_all(dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    let tar = dest.with_extension("tar");
    cmd::run(
        Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["archive", "--format=tar", "--output"])
            .arg(&tar)
            .arg(commit),
    )?;
    cmd::run(Command::new("tar").arg("-xf").arg(&tar).arg("-C").arg(dest))?;
    fs::remove_file(&tar).map_err(|e| format!("{}: {e}", tar.display()))
}

/// `podman run --rm` of `script` under `bash -euo pipefail` in `image`, with
/// each host directory mounted at its guest path (which may carry `:ro`).
fn podman_script(image: &str, mounts: &[(&Path, &str)], script: &str) -> Command {
    let mut command = Command::new("podman");
    command.args(["run", "--rm"]);
    for (host, guest) in mounts {
        command
            .arg("--volume")
            .arg(format!("{}:{guest}", host.display()));
    }
    command
        .arg(image)
        .args(["bash", "-euo", "pipefail", "-c", script]);
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|arg| (*arg).to_string()).collect()
    }

    #[test]
    fn the_ref_defaults_to_head() {
        let options = Options::parse(&args(&["flatpak"])).unwrap();
        assert_eq!(options.git_ref, "HEAD");
    }

    #[test]
    fn a_ref_can_be_named() {
        let options = Options::parse(&args(&["flatpak", "--ref", "v0.11.0"])).unwrap();
        assert_eq!(options.git_ref, "v0.11.0");
    }

    #[test]
    fn bad_arguments_are_refused() {
        assert!(Options::parse(&args(&[])).is_err());
        assert!(Options::parse(&args(&["rpm"])).is_err());
        assert!(Options::parse(&args(&["flatpak", "--ref"])).is_err());
        assert!(Options::parse(&args(&["flatpak", "--force"])).is_err());
        assert!(Options::parse(&args(&["flatpak", "--ref", "a", "b"])).is_err());
    }

    #[test]
    fn a_report_fails_when_any_step_failed() {
        let mut report = Report::default();
        report.pass("build");
        report.note("lint");
        assert!(report.verdict().is_ok());
        report.fail("launch", "exited early");
        assert!(report.verdict().is_err());
    }

    #[test]
    fn a_podman_script_mounts_each_directory_and_runs_under_strict_bash() {
        let command = podman_script("img", &[(Path::new("/host/in"), "/in:ro")], "true");
        let parts: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            parts,
            [
                "run",
                "--rm",
                "--volume",
                "/host/in:/in:ro",
                "img",
                "bash",
                "-euo",
                "pipefail",
                "-c",
                "true"
            ]
        );
    }
}
