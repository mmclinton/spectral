//! The install check every format shares: install into a fresh container,
//! through the package manager for Arch and deb, so only the declared
//! dependencies arrive, and with `flatpak install` for the Flatpak bundle;
//! ask the binary for its version, create the virtual mic on a headless
//! PipeWire with a test tone as its input, then launch the GUI headless
//! against X11 and against Wayland and wait for its window. Between them the
//! two launches load every library winit dlopens. Every step after the
//! install runs as a desktop user, through `scripts/session.sh`.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use super::{Build, PULL, Report, cmd, fresh_dir};

/// The display servers live in an image of their own: installed beside the
/// package, they would bring in the very libraries under test (Xvfb alone
/// pulls in libX11). `xwininfo` comes along to look for the X11 window.
const DISPLAY_IMAGE: &str = "localhost/spectral-xtask-display";
const DISPLAY_CONTAINERFILE: &str = "FROM docker.io/library/ubuntu:24.04
RUN apt-get update \\
 && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends xvfb weston x11-utils \\
 && rm -rf /var/lib/apt/lists/*
";

/// Stands in for a desktop login; every step after the install runs
/// through it, as the uid the launch pods map the host user to.
const SESSION_SCRIPT: &str = include_str!("../../scripts/session.sh");

/// Judges whether the installed package creates the virtual mic, and says
/// what is missing when it does not.
const MIC_CHECK_SCRIPT: &str = include_str!("../../scripts/mic-check.sh");

/// Maps the host user to the uid `session.sh` runs as, so the directory a
/// launch shares with its display server belongs to the desktop user in
/// both containers, whatever the host user's own uid.
const USERNS: &str = "--userns=keep-id:uid=1000,gid=1000";

/// How long the window may take to appear. The GUI opens it only after
/// trying the daemon, which with no PipeWire in the container can take all
/// of the client's connect timeout (`DAEMON_CONNECT_TIMEOUT` in
/// `src/platform/linux/ipc.rs`) first.
const WINDOW_TIMEOUT: Duration = Duration::from_secs(20);

/// How long the GUI must keep running once its window is up.
const SETTLE: Duration = Duration::from_secs(2);

/// The window's title, which both ways of spotting the window look for.
const TITLE: &str = "Spectral";

/// What the GUI prints when it cannot load a library it needs, or panics.
const LOAD_ERRORS: [&str; 3] = [
    "cannot open shared object",
    "error while loading shared libraries",
    "panicked",
];

/// What the install check needs to know about a format.
pub struct Target<'a> {
    /// Names this format's containers and image: `flatpak`, `arch` or
    /// `deb`.
    pub name: &'a str,
    /// A fresh image of the system the package is installed on.
    pub base_image: &'a str,
    /// Installs the package in `/pkg` with the tool a user would install it
    /// with, which brings in the declared dependencies and nothing else.
    pub install_script: &'a str,
    /// Runs Spectral in the installed system, as a user would type it.
    pub command: &'a [&'a str],
    /// `podman run` options every step after the install needs.
    pub podman_options: &'a [&'a str],
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
    version(build, target, &image.name, report);
    mic(target, &image.name, report);
    build_image(build, DISPLAY_IMAGE, DISPLAY_CONTAINERFILE)?;
    for display in [Display::X11, Display::Wayland] {
        launch(build, target, &image.name, display, report);
    }
    Ok(())
}

/// Installs the package in a fresh container and keeps the result as an
/// image, so each launch starts from the same installed system.
///
/// The container and image are named after the format alone, as the pods
/// are, and each replaces what already carries its name: an interrupted
/// run skips every `Drop`, so the next run is what clears its leftovers.
/// Two runs of one format cannot overlap in any case; they share
/// `target/packages/<format>`.
fn install(build: &Build, target: &Target) -> Result<Image, String> {
    let name = format!("localhost/spectral-xtask-check-{}", target.name);
    let container = format!("spectral-xtask-{}-install", target.name);
    // An interrupted run's image: committing over its tag would leave it
    // behind, untagged.
    cmd::output(Command::new("podman").args(["rmi", "--force", "--ignore", name.as_str()]))?;
    let installed = cmd::run(
        Command::new("podman")
            .args(["run", PULL, "--replace", "--name", container.as_str()])
            .arg("--volume")
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

/// `podman run` of `args` in `image` through `session.sh`, with `options`
/// before the image. It starts as root, which a pod's mapped user would not,
/// because `session.sh` starts the system bus before it drops to the user.
fn in_session(options: &[&str], image: &str, args: &[&str]) -> Command {
    let mut command = Command::new("podman");
    command
        .args(["run", "--user", "0"])
        .args(options)
        .arg(image)
        .args(["bash", "-euo", "pipefail", "-c", SESSION_SCRIPT, "session"])
        .args(args);
    command
}

fn version(build: &Build, target: &Target, image: &str, report: &mut Report) {
    let step = "spectral --version";
    let expected = format!("spectral {}", build.version);
    let options = [&["--rm"][..], target.podman_options].concat();
    let args = [target.command, &["--version"][..]].concat();
    match cmd::capture(&mut in_session(&options, image, &args)) {
        // Any line: podman and the session's buses can add their own output.
        Ok((Some(0), text)) if text.lines().any(|line| line.trim() == expected) => {
            report.pass(step);
        }
        Ok((_, text)) => report.fail(step, format!("expected `{expected}`, got: {}", text.trim())),
        Err(e) => report.fail(step, e),
    }
}

/// Whether the package does its job: with PipeWire, WirePlumber and a test
/// tone in the container, `enable` brings up a virtual mic that is running,
/// linked at both ends and the default source.
fn mic(target: &Target, image: &str, report: &mut Report) {
    let step = "creates the virtual mic";
    let options = [&["--rm"][..], target.podman_options].concat();
    match cmd::capture(&mut in_session(&options, image, &mic_check_args(target))) {
        Ok((Some(0), _)) => report.pass(step),
        Ok((_, text)) => report.fail(step, tail(&text)),
        Err(e) => report.fail(step, e),
    }
}

fn mic_check_args<'a>(target: &Target<'a>) -> Vec<&'a str> {
    let mut args = vec![
        "bash",
        "-euo",
        "pipefail",
        "-c",
        MIC_CHECK_SCRIPT,
        "mic-check",
    ];
    args.extend_from_slice(target.command);
    args
}

/// Builds the image `tag` from `containerfile`; podman's layer cache makes a
/// repeat build cost nothing until the Containerfile or its base changes.
pub fn build_image(build: &Build, tag: &str, containerfile: &str) -> Result<(), String> {
    let context = build
        .out
        .join("images")
        .join(tag.rsplit_once('/').map_or(tag, |(_, name)| name));
    fresh_dir(&context)?;
    fs::write(context.join("Containerfile"), containerfile)
        .map_err(|e| format!("{}: {e}", context.display()))?;
    cmd::output(
        Command::new("podman")
            .args(["build", PULL, "--quiet", "--tag", tag])
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
            Self::Wayland => "/run/user/1000",
        }
    }

    /// The server's socket, inside the shared directory.
    fn socket(self) -> &'static str {
        match self {
            Self::X11 => "X99",
            Self::Wayland => "wayland-1",
        }
    }

    /// Points the GUI at the server. `session.sh` sets `XDG_RUNTIME_DIR` to
    /// `/run/user/1000`, where the Wayland launch mounts the shared
    /// directory; `WAYLAND_DEBUG` makes the client log the protocol, which
    /// is how the Wayland window is spotted.
    fn client_env(self) -> &'static [&'static str] {
        match self {
            Self::X11 => &["DISPLAY=:99"],
            Self::Wayland => &["WAYLAND_DISPLAY=wayland-1", "WAYLAND_DEBUG=1"],
        }
    }

    /// Whether the GUI's window is up: on X11 the server lists it, on
    /// Wayland the client's protocol log shows its toplevel getting the
    /// title. Either needs winit to have loaded its libraries first.
    fn is_window_shown(self, server: &str, client: &str) -> Result<bool, String> {
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
fn launch(build: &Build, target: &Target, image: &str, display: Display, report: &mut Report) {
    let step = format!("GUI opens its window on {}", display.name());
    match try_launch(build, target, image, display) {
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

fn try_launch(
    build: &Build,
    target: &Target,
    image: &str,
    display: Display,
) -> Result<(Launch, String), String> {
    let pod = Pod::create(&format!("spectral-xtask-{}-{}", target.name, display.tag()))?;
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
            .args(["--env", "XDG_RUNTIME_DIR=/run/user/1000", DISPLAY_IMAGE])
            .args(display.server()),
    )?;
    wait_for(&shared.join(display.socket()))?;

    // Detached and kept after it exits, so its state and output can be
    // read while it runs and after; the pod removes it.
    let mut options = vec![
        "--detach",
        "--name",
        client.as_str(),
        "--pod",
        pod.name.as_str(),
        "--volume",
        volume.as_str(),
    ];
    for variable in display.client_env() {
        options.extend(["--env", variable]);
    }
    options.extend_from_slice(target.podman_options);
    cmd::output(&mut in_session(&options, image, target.command))?;

    let deadline = Instant::now() + WINDOW_TIMEOUT;
    let launch = loop {
        if display.is_window_shown(&server, &client)? {
            thread::sleep(SETTLE);
            break if is_running(&client)? {
                Launch::Started
            } else {
                Launch::Exited
            };
        }
        if !is_running(&client)? {
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

fn is_running(container: &str) -> Result<bool, String> {
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
/// shared-memory path works as on a desktop. Its user namespace maps the
/// host user to uid 1000 (`USERNS`).
struct Pod {
    name: String,
}

impl Pod {
    /// Creates the pod, first removing one an interrupted run left under
    /// the same name.
    fn create(name: &str) -> Result<Self, String> {
        Self::remove(name)?;
        cmd::output(Command::new("podman").args(["pod", "create", USERNS, "--name", name]))?;
        Ok(Self {
            name: name.to_string(),
        })
    }

    /// Removes the pod and its containers; a pod that is not there is no
    /// error.
    fn remove(name: &str) -> Result<(), String> {
        // --time 0: nothing in the pod needs a graceful stop.
        cmd::output(
            Command::new("podman").args(["pod", "rm", "--force", "--ignore", "--time", "0", name]),
        )
        .map(drop)
    }
}

impl Drop for Pod {
    fn drop(&mut self) {
        if let Err(e) = Self::remove(&self.name) {
            eprintln!("warning: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEB: Target<'static> = Target {
        name: "deb",
        base_image: "img",
        install_script: "true",
        command: &["spectral"],
        podman_options: &[],
    };

    #[test]
    fn the_mic_check_runs_the_targets_own_command() {
        let flatpak = Target {
            command: &["flatpak", "run", "app.id"],
            ..DEB
        };
        assert_eq!(
            mic_check_args(&flatpak),
            [
                "bash",
                "-euo",
                "pipefail",
                "-c",
                MIC_CHECK_SCRIPT,
                "mic-check",
                "flatpak",
                "run",
                "app.id"
            ]
        );
    }

    fn parts(command: &Command) -> Vec<String> {
        command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn a_session_command_starts_as_root_and_hands_its_arguments_to_session_sh() {
        let command = in_session(&["--rm"], "img", &["spectral", "--version"]);
        assert_eq!(
            parts(&command),
            [
                "run",
                "--user",
                "0",
                "--rm",
                "img",
                "bash",
                "-euo",
                "pipefail",
                "-c",
                SESSION_SCRIPT,
                "session",
                "spectral",
                "--version"
            ]
        );
    }
}
