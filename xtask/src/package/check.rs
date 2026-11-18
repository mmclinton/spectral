//! The install check the Arch and deb packages share: install into a fresh
//! container through the package manager, so only the declared
//! dependencies arrive, ask the binary for its version, then launch the GUI
//! headless against X11 and against Wayland. Between them the two launches
//! load every library winit dlopens.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use super::{Build, Report, cmd, fresh_dir};

/// The display servers live in an image of their own: installed beside the
/// package, they would bring in the very libraries under test (Xvfb alone
/// pulls in libX11).
const DISPLAY_IMAGE: &str = "localhost/spectral-xtask-display";
const DISPLAY_CONTAINERFILE: &str = "FROM docker.io/library/ubuntu:24.04
RUN apt-get update \\
 && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends xvfb weston \\
 && rm -rf /var/lib/apt/lists/*
";

/// How long the GUI must stay up to count as started.
const LAUNCH_SECONDS: u32 = 8;

/// What the GUI prints when a library it needs cannot be loaded.
const LOAD_ERRORS: [&str; 3] = [
    "cannot open shared object",
    "error while loading shared libraries",
    "panicked",
];

pub struct Target<'a> {
    /// Names this format's containers and image: `arch` or `deb`.
    pub name: &'a str,
    /// A fresh image of the distribution the package is for.
    pub base_image: &'a str,
    /// Installs the package in `/pkg` with the distribution's package
    /// manager, which resolves the declared dependencies and nothing else.
    pub install_script: &'a str,
}

pub fn run(build: &Build, target: &Target, report: &mut Report) -> Result<(), String> {
    let image = match install(build, target) {
        Ok(image) => {
            report.pass("installs with only its declared dependencies");
            image
        }
        Err(e) => {
            report.fail("install", e);
            return Ok(());
        }
    };
    version(build, &image.name, report);
    display_image(build)?;
    for display in [Display::X11, Display::Wayland] {
        launch(build, &image.name, display, report);
    }
    Ok(())
}

/// Installs the package in a fresh container and keeps the result as an
/// image, so each launch starts from the same installed system.
fn install(build: &Build, target: &Target) -> Result<Image, String> {
    let name = format!("localhost/spectral-xtask-check-{}", target.name);
    let container = format!(
        "spectral-xtask-install-{}-{}",
        target.name,
        std::process::id()
    );
    let installed = cmd::run(
        Command::new("podman")
            .args(["run", "--name", container.as_str(), "--volume"])
            .arg(format!("{}:/pkg:ro", build.out.join("pkg").display()))
            .arg(target.base_image)
            .args(["bash", "-euo", "pipefail", "-c", target.install_script]),
    );
    let committed = installed.and_then(|()| {
        cmd::output(Command::new("podman").args([
            "commit",
            "--quiet",
            container.as_str(),
            name.as_str(),
        ]))
        .map(|_| Image { name })
    });
    let removed =
        cmd::output(Command::new("podman").args(["rm", "--force", container.as_str()])).map(drop);
    let image = committed?;
    removed?;
    Ok(image)
}

/// The installed system, removed when dropped: committing onto the same
/// tag on every run would otherwise leave each earlier image behind,
/// untagged, at several hundred megabytes apiece.
struct Image {
    name: String,
}

impl Drop for Image {
    fn drop(&mut self) {
        if let Err(e) =
            cmd::output(Command::new("podman").args(["rmi", "--force", self.name.as_str()]))
        {
            eprintln!("warning: {e}");
        }
    }
}

fn version(build: &Build, image: &str, report: &mut Report) {
    let step = "spectral --version";
    let expected = format!("spectral {}", build.version);
    match cmd::capture(Command::new("podman").args(["run", "--rm", image, "spectral", "--version"]))
    {
        Ok((Some(0), text)) if text.trim() == expected => report.pass(step),
        Ok((_, text)) => report.fail(step, format!("expected `{expected}`, got: {}", text.trim())),
        Err(e) => report.fail(step, e),
    }
}

/// Builds the display-server image; podman's layer cache makes a repeat
/// build cost nothing until the Containerfile or its base changes.
fn display_image(build: &Build) -> Result<(), String> {
    let context = build.out.join("display-image");
    fresh_dir(&context)?;
    fs::write(context.join("Containerfile"), DISPLAY_CONTAINERFILE)
        .map_err(|e| format!("{}: {e}", context.display()))?;
    cmd::output(
        Command::new("podman")
            .args(["build", "--quiet", "--tag", DISPLAY_IMAGE])
            .arg(&context),
    )
    .map(drop)
}

#[derive(Debug, Clone, Copy)]
enum Display {
    X11,
    Wayland,
}

impl Display {
    fn name(self) -> &'static str {
        match self {
            Self::X11 => "X11 (Xvfb)",
            Self::Wayland => "Wayland (headless Weston)",
        }
    }

    fn tag(self) -> &'static str {
        match self {
            Self::X11 => "x11",
            Self::Wayland => "wayland",
        }
    }

    /// The server, run in the display image.
    fn server(self) -> &'static [&'static str] {
        match self {
            Self::X11 => &[
                "Xvfb",
                ":99",
                "-screen",
                "0",
                "1280x800x24",
                "-nolisten",
                "tcp",
                "-ac",
            ],
            Self::Wayland => &[
                "weston",
                "--backend=headless",
                "--renderer=pixman",
                "--socket=wayland-1",
                "--idle-time=0",
            ],
        }
    }

    /// The directory both containers share, at this path inside each.
    fn shared_dir(self) -> &'static str {
        match self {
            Self::X11 => "/tmp/.X11-unix",
            Self::Wayland => "/run/xdg",
        }
    }

    /// The server's socket, inside the shared directory.
    fn socket(self) -> &'static str {
        match self {
            Self::X11 => "X99",
            Self::Wayland => "wayland-1",
        }
    }

    /// Points the GUI at the server. `XDG_RUNTIME_DIR` also holds the
    /// daemon's socket; on Wayland it must be the shared directory.
    fn client_env(self) -> [&'static str; 2] {
        match self {
            Self::X11 => ["DISPLAY=:99", "XDG_RUNTIME_DIR=/run/xdg"],
            Self::Wayland => ["WAYLAND_DISPLAY=wayland-1", "XDG_RUNTIME_DIR=/run/xdg"],
        }
    }
}

/// The GUI counts as started when it is still running after
/// [`LAUNCH_SECONDS`] (so `timeout` killed it, exit 124) and printed no
/// load error. With no PipeWire session the daemon cannot create the mic;
/// the GUI should stay up anyway, showing its error state.
fn launch(build: &Build, image: &str, display: Display, report: &mut Report) {
    let step = format!("GUI starts on {}", display.name());
    match try_launch(build, image, display) {
        Ok((Some(124), output)) if !LOAD_ERRORS.iter().any(|error| output.contains(error)) => {
            report.pass(step);
        }
        Ok((code, output)) => report.fail(
            step,
            format!("exit {code:?} within {LAUNCH_SECONDS}s or a load error:\n{output}"),
        ),
        Err(e) => report.fail(step, e),
    }
}

fn try_launch(
    build: &Build,
    image: &str,
    display: Display,
) -> Result<(Option<i32>, String), String> {
    let pod = Pod::create(&format!(
        "spectral-xtask-{}-{}",
        std::process::id(),
        display.tag()
    ))?;
    let shared = build.out.join(format!("display-{}", display.tag()));
    fresh_dir(&shared)?;
    fs::set_permissions(&shared, fs::Permissions::from_mode(0o700))
        .map_err(|e| format!("{}: {e}", shared.display()))?;
    let volume = format!("{}:{}", shared.display(), display.shared_dir());

    cmd::output(
        Command::new("podman")
            .args([
                "run",
                "--detach",
                "--rm",
                "--pod",
                pod.name.as_str(),
                "--volume",
                volume.as_str(),
            ])
            .args(["--env", "XDG_RUNTIME_DIR=/run/xdg", DISPLAY_IMAGE])
            .args(display.server()),
    )?;
    wait_for(&shared.join(display.socket()))?;

    let script = format!("mkdir -p -m 700 \"$XDG_RUNTIME_DIR\"; timeout {LAUNCH_SECONDS} spectral");
    let mut client = Command::new("podman");
    client.args([
        "run",
        "--rm",
        "--pod",
        pod.name.as_str(),
        "--volume",
        volume.as_str(),
    ]);
    for variable in display.client_env() {
        client.args(["--env", variable]);
    }
    client.args([image, "bash", "-c", script.as_str()]);
    cmd::capture(&mut client)
}

fn wait_for(socket: &Path) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !socket.exists() {
        if Instant::now() > deadline {
            return Err(format!(
                "the display server never created {}",
                socket.display()
            ));
        }
        thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

/// A podman pod, removed with its containers when dropped. The display
/// server and the GUI share its network and IPC namespaces, so X11's
/// shared-memory path works as on a desktop.
struct Pod {
    name: String,
}

impl Pod {
    fn create(name: &str) -> Result<Self, String> {
        cmd::output(Command::new("podman").args(["pod", "create", "--name", name]))?;
        Ok(Self {
            name: name.to_string(),
        })
    }
}

impl Drop for Pod {
    fn drop(&mut self) {
        if let Err(e) =
            cmd::output(Command::new("podman").args(["pod", "rm", "--force", self.name.as_str()]))
        {
            eprintln!("warning: {e}");
        }
    }
}
