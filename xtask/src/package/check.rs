//! The install check the Arch and deb packages share: install into a fresh
//! container through the package manager, so only the declared
//! dependencies arrive, ask the binary for its version, then launch the GUI
//! headless against X11 and against Wayland and wait for its window.
//! Between them the two launches load every library winit dlopens.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use super::{Build, Report, cmd, fresh_dir};

/// The display servers live in an image of their own: installed beside the
/// package, they would bring in the very libraries under test (Xvfb alone
/// pulls in libX11). `xwininfo` comes along to look for the X11 window.
const DISPLAY_IMAGE: &str = "localhost/spectral-xtask-display";
const DISPLAY_CONTAINERFILE: &str = "FROM docker.io/library/ubuntu:24.04
RUN apt-get update \\
 && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends xvfb weston x11-utils \\
 && rm -rf /var/lib/apt/lists/*
";

/// How long the window may take to appear. The GUI opens it only after
/// trying the daemon, which with no PipeWire in the container can take the
/// connection's whole five-second timeout first.
const WINDOW_TIMEOUT: Duration = Duration::from_secs(20);

/// How long the GUI must keep running once its window is up.
const SETTLE: Duration = Duration::from_secs(2);

/// The window's title, which both ways of spotting the window look for.
const TITLE: &str = "Spectral";

/// What the GUI prints when a library it needs cannot be loaded.
const LOAD_ERRORS: [&str; 3] = [
    "cannot open shared object",
    "error while loading shared libraries",
    "panicked",
];

/// What the install check needs to know about a distribution.
pub struct Target<'a> {
    /// Names this format's containers and image: `arch` or `deb`.
    pub name: &'a str,
    /// A fresh image of the distribution the package is for.
    pub base_image: &'a str,
    /// Installs the package in `/pkg` with the distribution's package
    /// manager, which resolves the declared dependencies and nothing else.
    pub install_script: &'a str,
}

/// Runs the install check, recording each step in `report`; only a failure
/// to run it at all is an error.
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
        // Any line: podman can add its own warnings to the output.
        Ok((Some(0), text)) if text.lines().any(|line| line.trim() == expected) => {
            report.pass(step);
        }
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
    /// daemon's socket; on Wayland it must be the shared directory, and
    /// `WAYLAND_DEBUG` makes the client log the protocol, which is how the
    /// Wayland window is spotted.
    fn client_env(self) -> &'static [&'static str] {
        match self {
            Self::X11 => &["DISPLAY=:99", "XDG_RUNTIME_DIR=/run/xdg"],
            Self::Wayland => &[
                "WAYLAND_DISPLAY=wayland-1",
                "XDG_RUNTIME_DIR=/run/xdg",
                "WAYLAND_DEBUG=1",
            ],
        }
    }

    /// Whether the GUI's window is up: on X11 the server lists it, on
    /// Wayland the client's protocol log shows its toplevel getting the
    /// title. Either needs winit to have loaded its libraries first.
    fn window_shown(self, server: &str, client: &str) -> Result<bool, String> {
        match self {
            Self::X11 => {
                let tree = cmd::output(Command::new("podman").args([
                    "exec",
                    server,
                    "xwininfo",
                    "-display",
                    ":99",
                    "-root",
                    "-children",
                ]))?;
                Ok(tree.contains(&format!("\"{TITLE}\"")))
            }
            Self::Wayland => {
                let (_, log) = cmd::capture(Command::new("podman").args(["logs", client]))?;
                let set_title = format!("set_title(\"{TITLE}\")");
                Ok(log
                    .lines()
                    .any(|line| line.contains("xdg_toplevel") && line.contains(&set_title)))
            }
        }
    }
}

enum Launch {
    /// The window appeared and the GUI was still running a moment later.
    Started,
    /// The GUI was still running, but no window appeared in time.
    NoWindow,
    /// The GUI exited.
    Exited,
}

/// The GUI counts as started once its window appears and it is still
/// running [`SETTLE`] later, with no load error in its output. With no
/// PipeWire session the daemon cannot create the mic; the GUI should open
/// anyway, showing its error state.
fn launch(build: &Build, image: &str, display: Display, report: &mut Report) {
    let step = format!("GUI opens its window on {}", display.name());
    match try_launch(build, image, display) {
        Ok((Launch::Started, output)) if !LOAD_ERRORS.iter().any(|e| output.contains(e)) => {
            report.pass(step);
        }
        Ok((Launch::Started, output)) => {
            report.fail(step, format!("a load error:\n{}", tail(&output)));
        }
        Ok((Launch::NoWindow, output)) => report.fail(
            step,
            format!(
                "no window within {}s:\n{}",
                WINDOW_TIMEOUT.as_secs(),
                tail(&output)
            ),
        ),
        Ok((Launch::Exited, output)) => {
            report.fail(step, format!("exited:\n{}", tail(&output)));
        }
        Err(e) => report.fail(step, e),
    }
}

fn try_launch(build: &Build, image: &str, display: Display) -> Result<(Launch, String), String> {
    let pod = Pod::create(&format!(
        "spectral-xtask-{}-{}",
        std::process::id(),
        display.tag()
    ))?;
    let server = format!("{}-server", pod.name);
    let client = format!("{}-client", pod.name);
    let shared = build.out.join(format!("display-{}", display.tag()));
    fresh_dir(&shared)?;
    fs::set_permissions(&shared, fs::Permissions::from_mode(0o700))
        .map_err(|e| format!("{}: {e}", shared.display()))?;
    let volume = format!("{}:{}", shared.display(), display.shared_dir());

    cmd::output(
        Command::new("podman")
            .args(["run", "--detach", "--name", server.as_str()])
            .args(["--pod", pod.name.as_str(), "--volume", volume.as_str()])
            .args(["--env", "XDG_RUNTIME_DIR=/run/xdg", DISPLAY_IMAGE])
            .args(display.server()),
    )?;
    wait_for(&shared.join(display.socket()))?;

    // Detached and kept after it exits, so its state and output can be
    // read while it runs and after; the pod removes it.
    let mut command = Command::new("podman");
    command
        .args(["run", "--detach", "--name", client.as_str()])
        .args(["--pod", pod.name.as_str(), "--volume", volume.as_str()]);
    for variable in display.client_env() {
        command.args(["--env", variable]);
    }
    command.args([
        image,
        "bash",
        "-c",
        "mkdir -p -m 700 \"$XDG_RUNTIME_DIR\" && exec spectral",
    ]);
    cmd::output(&mut command)?;

    let deadline = Instant::now() + WINDOW_TIMEOUT;
    let launch = loop {
        if display.window_shown(&server, &client)? {
            thread::sleep(SETTLE);
            break if running(&client)? {
                Launch::Started
            } else {
                Launch::Exited
            };
        }
        if !running(&client)? {
            break Launch::Exited;
        }
        if Instant::now() > deadline {
            break Launch::NoWindow;
        }
        thread::sleep(Duration::from_millis(250));
    };
    let (_, output) = cmd::capture(Command::new("podman").args(["logs", client.as_str()]))?;
    Ok((launch, output))
}

fn running(container: &str) -> Result<bool, String> {
    let state = cmd::output(Command::new("podman").args([
        "inspect",
        "--format",
        "{{.State.Running}}",
        container,
    ]))?;
    Ok(state.trim() == "true")
}

/// The last lines of a container's output; the Wayland launch's protocol
/// log makes the whole of it too long for a report.
fn tail(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    lines[lines.len().saturating_sub(40)..].join("\n")
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
        // --time 0: nothing in the pod needs a graceful stop.
        if let Err(e) = cmd::output(Command::new("podman").args([
            "pod",
            "rm",
            "--force",
            "--time",
            "0",
            self.name.as_str(),
        ])) {
            eprintln!("warning: {e}");
        }
    }
}
