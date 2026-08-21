//! Headless subcommands for the `spectral` binary. Bare `spectral` launches
//! the GUI; everything here runs against the daemon socket (or the PipeWire
//! registry directly) without touching Slint.

use crate::daemon_launch;
use spectral_lib::platform::audio_devices;
use spectral_lib::platform::ipc::{
    DaemonClient, DaemonCommand, DaemonResponse, DaemonState, PROTOCOL_VERSION,
    wait_for_daemon_exit,
};
use spectral_lib::state::{Status, is_valid_mic_name, mic_name_rule};
use std::ffi::OsString;
use std::process::ExitCode;

/// The action selected by the argument list; `Gui` is the flagless default.
#[derive(Debug, PartialEq)]
pub enum CliCommand {
    Gui,
    Enable {
        device_id: Option<u32>,
        threshold: Option<i32>,
        mic_name: Option<String>,
        rumble_filter: Option<bool>,
        auto_level: Option<bool>,
    },
    Disable,
    Status,
    Devices,
    Shutdown,
    Help,
    Version,
}

// The three codes documented in `help()` and docs/CLI.md. Only
// EXIT_USAGE_ERROR is single-sourced in the strict sense: it has no std
// spelling, so `main` returns it and `help()` interpolates it. The other
// two exist to render that table — `run` returns std's own
// `ExitCode::SUCCESS`/`FAILURE`, which is the idiomatic spelling and worth
// keeping at every call site. Markdown cannot interpolate, so docs/CLI.md
// spells all three out. Nothing structural ties the two vocabularies
// together, so a test below pins the equivalence instead.

/// Success; mirrors `ExitCode::SUCCESS`.
pub const EXIT_SUCCESS: u8 = 0;
/// Runtime failure; mirrors `ExitCode::FAILURE`.
pub const EXIT_FAILURE: u8 = 1;
/// Usage error — argument parsing failed before any command ran.
pub const EXIT_USAGE_ERROR: u8 = 2;

/// Built at run time so the `--mic-name` line quotes [`mic_name_rule`]
/// instead of restating it; a relaxed rule would otherwise leave `--help`
/// advertising a constraint the parser no longer applies.
fn help() -> String {
    let rule = mic_name_rule();
    let version = env!("CARGO_PKG_VERSION");
    format!(
        "\
spectral {version} — real-time noise suppression for PipeWire

Usage:  spectral [COMMAND]
        spectral                  open the control panel (GUI)

Commands:
  enable      Create the virtual microphone and start denoising
  disable     Tear down the virtual microphone
  status      Show the daemon's state
  devices     List input devices with their current PipeWire ids
  shutdown    Stop the daemon

Flags for `enable` — all optional; an omitted flag keeps its last value, so a
bare `spectral enable` reuses the whole remembered setup:
      --device <ID>             Input device to capture; see `spectral devices`
      --threshold <0-100>       Voice gate threshold; 0 = denoise only
      --mic-name <NAME>         Virtual mic name;
                                {rule}
      --rumble-filter <on|off>  Cut rumble below ~90 Hz before denoising
      --auto-level <on|off>     Even out volume; peak limiter as the ceiling

Options:
  -h, --help     Show this help
  -V, --version  Show the version

Examples:
  spectral devices               find an input device id
  spectral enable                start with the remembered settings
  spectral enable --threshold 0  denoise only, no voice gate

Device ids are scoped to the running PipeWire instance; the daemon remembers
your device by node name, so a flagless `enable` survives reboots and
PipeWire restarts.

Exit codes:
  {EXIT_SUCCESS}  success (including a no-op disable or shutdown)
  {EXIT_FAILURE}  runtime failure
  {EXIT_USAGE_ERROR}  usage error"
    )
}

/// Parses the arguments after argv\[0\]; an `Err` is a usage-error message
/// destined for exit code 2. Both `--flag value` and `--flag=value` forms
/// are accepted.
pub fn parse<A: IntoIterator<Item = OsString>>(args: A) -> Result<CliCommand, String> {
    let mut args = args.into_iter().map(|arg| {
        arg.into_string()
            .map_err(|bad| format!("invalid UTF-8 in argument {bad:?}"))
    });

    let Some(first) = args.next() else {
        return Ok(CliCommand::Gui);
    };

    let command = match first?.as_str() {
        "enable" => parse_enable(&mut args)?,
        "disable" => CliCommand::Disable,
        "status" => CliCommand::Status,
        "devices" => CliCommand::Devices,
        "shutdown" => CliCommand::Shutdown,
        "--help" | "-h" => CliCommand::Help,
        "--version" | "-V" => CliCommand::Version,
        other => return Err(format!("unknown command '{other}'")),
    };

    match args.next() {
        None => Ok(command),
        Some(extra) => Err(format!("unexpected argument '{}'", extra?)),
    }
}

fn parse_enable<I>(args: &mut I) -> Result<CliCommand, String>
where
    I: Iterator<Item = Result<String, String>>,
{
    let mut device_id: Option<u32> = None;
    let mut threshold: Option<i32> = None;
    let mut mic_name: Option<String> = None;
    let mut rumble_filter: Option<bool> = None;
    let mut auto_level: Option<bool> = None;

    while let Some(arg) = args.next() {
        let arg = arg?;
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) => (flag.to_owned(), Some(value.to_owned())),
            None => (arg, None),
        };
        match flag.as_str() {
            "--device" => {
                if device_id.is_some() {
                    return Err("duplicate flag --device".to_string());
                }
                let value = take_value(args, inline, "--device")?;
                device_id =
                    Some(value.parse().map_err(|_| {
                        format!("invalid --device '{value}': expected a numeric id")
                    })?);
            }
            "--threshold" => {
                if threshold.is_some() {
                    return Err("duplicate flag --threshold".to_string());
                }
                let value = take_value(args, inline, "--threshold")?;
                let parsed: i32 = value
                    .parse()
                    .map_err(|_| format!("invalid --threshold '{value}': expected 0-100"))?;
                if !(0..=100).contains(&parsed) {
                    return Err(format!("invalid --threshold '{value}': expected 0-100"));
                }
                threshold = Some(parsed);
            }
            "--mic-name" => {
                if mic_name.is_some() {
                    return Err("duplicate flag --mic-name".to_string());
                }
                let value = take_value(args, inline, "--mic-name")?;
                if !is_valid_mic_name(&value) {
                    let rule = mic_name_rule();
                    return Err(format!("invalid --mic-name '{value}': {rule}"));
                }
                mic_name = Some(value);
            }
            "--rumble-filter" => {
                if rumble_filter.is_some() {
                    return Err("duplicate flag --rumble-filter".to_string());
                }
                let value = take_value(args, inline, "--rumble-filter")?;
                rumble_filter = Some(parse_toggle(&value, "--rumble-filter")?);
            }
            "--auto-level" => {
                if auto_level.is_some() {
                    return Err("duplicate flag --auto-level".to_string());
                }
                let value = take_value(args, inline, "--auto-level")?;
                auto_level = Some(parse_toggle(&value, "--auto-level")?);
            }
            other => return Err(format!("unexpected argument '{other}'")),
        }
    }

    Ok(CliCommand::Enable {
        device_id,
        threshold,
        mic_name,
        rumble_filter,
        auto_level,
    })
}

fn parse_toggle(value: &str, flag: &str) -> Result<bool, String> {
    match value {
        "on" => Ok(true),
        "off" => Ok(false),
        other => Err(format!("invalid {flag} '{other}': expected on|off")),
    }
}

fn take_value<I>(args: &mut I, inline: Option<String>, flag: &str) -> Result<String, String>
where
    I: Iterator<Item = Result<String, String>>,
{
    match inline {
        Some(value) => Ok(value),
        None => args
            .next()
            .transpose()?
            .ok_or_else(|| format!("{flag} requires a value")),
    }
}

/// Executes any non-GUI command, mapping outcomes to the exit codes the
/// help text documents.
pub fn run(command: CliCommand) -> ExitCode {
    match command {
        // Handled by main(); listed so the match stays exhaustive.
        CliCommand::Gui => ExitCode::SUCCESS,
        CliCommand::Help => {
            let text = help();
            println!("{text}");
            ExitCode::SUCCESS
        }
        CliCommand::Version => {
            println!("spectral {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        CliCommand::Devices => run_devices(),
        CliCommand::Enable {
            device_id,
            threshold,
            mic_name,
            rumble_filter,
            auto_level,
        } => run_enable(device_id, threshold, mic_name, rumble_filter, auto_level),
        CliCommand::Disable => run_disable(),
        CliCommand::Status => run_status(),
        CliCommand::Shutdown => run_shutdown(),
    }
}

fn run_devices() -> ExitCode {
    match audio_devices::get_input_devices() {
        Ok(devices) if devices.is_empty() => {
            println!("no input devices found");
            ExitCode::SUCCESS
        }
        Ok(devices) => {
            println!("{:>4}  {:>8}  NAME", "ID", "CHANNELS");
            for device in &devices {
                println!("{:>4}  {:>8}  {}", device.id, device.channels, device.name);
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("spectral: failed to list devices: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run_enable(
    device_id: Option<u32>,
    threshold: Option<i32>,
    mic_name: Option<String>,
    rumble_filter: Option<bool>,
    auto_level: Option<bool>,
) -> ExitCode {
    let (mut client, _) = match daemon_launch::connect_and_fetch_state() {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("spectral: cannot reach the daemon: {e}");
            return ExitCode::FAILURE;
        }
    };

    match client.send_command(DaemonCommand::Enable {
        device_id,
        mic_name,
        threshold,
        rumble_filter,
        auto_level,
    }) {
        Ok(DaemonResponse::Ok) => {
            println!("noise suppression enabled");
            ExitCode::SUCCESS
        }
        Ok(DaemonResponse::Error(e)) => {
            eprintln!("spectral: {e}");
            ExitCode::FAILURE
        }
        Ok(DaemonResponse::State(_)) => {
            eprintln!("spectral: unexpected response from daemon");
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("spectral: failed to enable: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run_disable() -> ExitCode {
    let mut client = match DaemonClient::connect() {
        Ok(client) => client,
        Err(_) => {
            println!("daemon not running; nothing to disable");
            return ExitCode::SUCCESS;
        }
    };

    match client.send_command(DaemonCommand::Disable) {
        Ok(DaemonResponse::Ok) => {
            println!("noise suppression disabled");
            ExitCode::SUCCESS
        }
        Ok(DaemonResponse::Error(e)) => {
            eprintln!("spectral: {e}");
            ExitCode::FAILURE
        }
        Ok(DaemonResponse::State(_)) => {
            eprintln!("spectral: unexpected response from daemon");
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("spectral: failed to disable: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run_status() -> ExitCode {
    let mut client = match DaemonClient::connect() {
        Ok(client) => client,
        Err(_) => {
            println!("daemon not running");
            return ExitCode::FAILURE;
        }
    };

    match client.send_command(DaemonCommand::GetState) {
        Ok(DaemonResponse::State(state)) if state.protocol_version != PROTOCOL_VERSION => {
            eprintln!("spectral: the daemon is an old version — `spectral enable` replaces it");
            ExitCode::FAILURE
        }
        Ok(DaemonResponse::State(state)) => {
            print_status(&state);
            ExitCode::SUCCESS
        }
        Ok(_) => {
            eprintln!("spectral: unexpected response from daemon");
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!(
                "spectral: the daemon did not answer (an old version?): {e} — `spectral enable` replaces it"
            );
            ExitCode::FAILURE
        }
    }
}

fn print_status(state: &DaemonState) {
    let status = match state.status {
        Status::Active => "Active",
        Status::Pending => "Pending",
        Status::Inactive => "Inactive",
    };
    println!("status:           {status}");
    println!("device:           {}", describe_device(state));
    println!("mic name:         {}", state.mic_name);
    println!("threshold:        {}%", state.threshold);
    println!(
        "rumble filter:    {}",
        if state.rumble_filter { "on" } else { "off" }
    );
    println!(
        "auto level:       {}",
        if state.auto_level { "on" } else { "off" }
    );
    if let Some(error) = &state.last_error {
        println!("last error:       {error}");
    }
}

/// Best-effort human description of the daemon's device; enumeration
/// failures degrade the label rather than failing `status`.
fn describe_device(state: &DaemonState) -> String {
    let devices = audio_devices::get_input_devices().unwrap_or_default();
    if let Some(id) = state.device_id
        && let Some(device) = devices.iter().find(|device| device.id == id)
    {
        return format!("{} (id {id})", device.name);
    }
    match &state.device_node_name {
        // The remembered id can be stale while the device is still present
        // under a new one; the node name is the durable handle.
        Some(name) => match audio_devices::find_node_id(name) {
            Ok(Some(id)) => format!("{name} (id {id})"),
            _ => format!("{name} (not currently present)"),
        },
        None => "none".to_string(),
    }
}

fn run_shutdown() -> ExitCode {
    let mut client = match DaemonClient::connect() {
        Ok(client) => client,
        Err(_) => {
            println!("daemon not running");
            return ExitCode::SUCCESS;
        }
    };

    match client.send_command(DaemonCommand::Shutdown) {
        Ok(DaemonResponse::Error(e)) => {
            eprintln!("spectral: {e}");
            return ExitCode::FAILURE;
        }
        Ok(_) => {}
        Err(e) => {
            eprintln!("spectral: failed to send shutdown: {e}");
            return ExitCode::FAILURE;
        }
    }

    match wait_for_daemon_exit() {
        Ok(()) => {
            println!("daemon stopped");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("spectral: daemon did not exit: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spectral_lib::state::MIC_NAME_MAX_LEN;

    fn p(args: &[&str]) -> Result<CliCommand, String> {
        parse(args.iter().map(OsString::from))
    }

    #[test]
    fn help_quotes_the_mic_name_rule_the_parser_enforces() {
        assert!(help().contains(&mic_name_rule()));
    }

    /// The tables render these constants, but `run` returns std's codes, so
    /// this is what keeps the two vocabularies in agreement. The literal 2
    /// is the contract: changing it has to be a deliberate edit here.
    #[test]
    fn exit_codes_match_the_std_constants_and_the_documented_values() {
        assert_eq!(ExitCode::from(EXIT_SUCCESS), ExitCode::SUCCESS);
        assert_eq!(ExitCode::from(EXIT_FAILURE), ExitCode::FAILURE);
        assert_eq!(EXIT_USAGE_ERROR, 2);
    }

    /// The two arms that reach `run` without touching the daemon socket.
    /// `Gui` is deliberately absent: `main` intercepts it before delegating,
    /// so its arm here is unreachable and pinning it would only suggest
    /// otherwise.
    #[test]
    fn help_and_version_exit_with_success() {
        assert_eq!(run(CliCommand::Help), ExitCode::SUCCESS);
        assert_eq!(run(CliCommand::Version), ExitCode::SUCCESS);
    }

    #[test]
    fn help_documents_all_three_exit_codes() {
        let text = help();
        let (_, table) = text
            .split_once("Exit codes:")
            .expect("--help must carry an exit-code table");
        for code in [EXIT_SUCCESS, EXIT_FAILURE, EXIT_USAGE_ERROR] {
            assert!(
                table.contains(&format!("\n  {code}  ")),
                "exit code {code} is missing from the help table"
            );
        }
    }

    /// docs/CLI.md is the published reference and part of the same frozen
    /// contract as the parser. Markdown cannot interpolate the constants the
    /// way [`help`] does, so this test is the only thing standing between a
    /// code change and a doc that silently keeps advertising the old number.
    #[test]
    fn the_cli_reference_documents_the_same_exit_codes() {
        let doc = include_str!("../docs/CLI.md");
        let (_, table) = doc
            .split_once("## Exit codes")
            .expect("docs/CLI.md must carry an exit-code table");
        for code in [EXIT_SUCCESS, EXIT_FAILURE, EXIT_USAGE_ERROR] {
            assert!(
                table.contains(&format!("| `{code}` |")),
                "exit code {code} is missing from the docs/CLI.md table"
            );
        }
    }

    /// The half of the frozen CLI surface no constant can render. The exit
    /// codes reach `help()` by interpolation, but the subcommand set and the
    /// `enable` flags are prose in two places — `help()` and docs/CLI.md —
    /// with the parser as the third copy. This table is the one list all
    /// three are checked against.
    const SUBCOMMANDS: [&str; 5] = ["enable", "disable", "status", "devices", "shutdown"];
    const ENABLE_FLAGS: [&str; 5] = [
        "--device",
        "--threshold",
        "--mic-name",
        "--rumble-filter",
        "--auto-level",
    ];
    /// Long options that belong to no subcommand. Not part of the frozen
    /// surface — they sit here so the reverse scan below has somewhere to
    /// put a `--` token that is legitimately not an `enable` flag. Short
    /// forms (`-h`, `-V`) go unchecked; the scan only looks at `--`.
    const GLOBAL_OPTIONS: [&str; 2] = ["--help", "--version"];

    /// Forces a new [`CliCommand`] to declare whether it is spelled as a
    /// subcommand: the match is exhaustive, so the variant will not compile
    /// until its arm names a word or says there is none.
    ///
    /// That is the entire guarantee, and it is weaker than it looks. Nothing
    /// iterates the variants, so a word named here does not have to join
    /// [`SUBCOMMANDS`], and the tests below — which walk that table, not this
    /// function — never see it. A new subcommand can still ship undocumented
    /// with the suite green. What the compile error buys is putting whoever
    /// adds one in front of this table; closing the gap for real needs
    /// variant iteration Rust does not offer.
    fn subcommand_word(command: &CliCommand) -> Option<&'static str> {
        match command {
            CliCommand::Enable { .. } => Some("enable"),
            CliCommand::Disable => Some("disable"),
            CliCommand::Status => Some("status"),
            CliCommand::Devices => Some("devices"),
            CliCommand::Shutdown => Some("shutdown"),
            CliCommand::Gui | CliCommand::Help | CliCommand::Version => None,
        }
    }

    #[test]
    fn every_subcommand_parses_and_is_documented_in_both_references() {
        let help = help();
        let doc = include_str!("../docs/CLI.md");
        for word in SUBCOMMANDS {
            let parsed = p(&[word]).unwrap_or_else(|e| panic!("`spectral {word}` must parse: {e}"));
            assert_eq!(subcommand_word(&parsed), Some(word));
            assert!(
                help.contains(&format!("\n  {word}")),
                "`{word}` is missing from the --help command list"
            );
            // Anchored to a line start, which is the usage block: `devices`
            // and `enable` also occur inside prose and a table cell, so an
            // unanchored search stayed green when the block dropped them and
            // only pinned the three words mentioned exactly once.
            assert!(
                doc.contains(&format!("\nspectral {word}")),
                "`{word}` is missing from the docs/CLI.md usage block"
            );
        }
    }

    #[test]
    fn every_enable_flag_is_recognized_and_documented_in_both_references() {
        let help = help();
        let doc = include_str!("../docs/CLI.md");
        for flag in ENABLE_FLAGS {
            // A flag the parser has dropped answers "unexpected argument"
            // instead, which is what separates this from a spelling check.
            assert_eq!(
                p(&["enable", flag]),
                Err(format!("{flag} requires a value")),
                "the parser no longer recognizes {flag}"
            );
            assert!(help.contains(flag), "{flag} is missing from --help");
            assert!(
                doc.contains(&format!("`{flag} ")),
                "{flag} is missing from the docs/CLI.md flag table"
            );
        }
    }

    /// The other direction: a flag added to either reference but not to
    /// [`ENABLE_FLAGS`] fails here, so the table cannot fall behind the prose
    /// the way docs/CLI.md fell behind the exit codes.
    #[test]
    fn neither_reference_documents_a_flag_the_table_does_not_carry() {
        let help = help();
        let doc = include_str!("../docs/CLI.md");
        for (source, text) in [("--help", help.as_str()), ("docs/CLI.md", doc)] {
            for word in text.split_whitespace() {
                let flag = word.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-');
                // Split on `=` the way `parse` does: both spellings name the
                // same flag, and without this, documenting the `--flag=value`
                // form the parser advertises would fail this test.
                let flag = flag.split_once('=').map_or(flag, |(name, _)| name);
                // Markdown rules (`---`) share the prefix but carry no name.
                if !flag.starts_with("--") || !flag.contains(|c: char| c.is_ascii_alphanumeric()) {
                    continue;
                }
                assert!(
                    ENABLE_FLAGS.contains(&flag) || GLOBAL_OPTIONS.contains(&flag),
                    "{source} documents {flag}, which is in neither ENABLE_FLAGS nor GLOBAL_OPTIONS"
                );
            }
        }
    }

    #[test]
    fn bare_invocation_is_gui() {
        assert_eq!(p(&[]), Ok(CliCommand::Gui));
    }

    #[test]
    fn help_and_version_forms() {
        assert_eq!(p(&["--help"]), Ok(CliCommand::Help));
        assert_eq!(p(&["-h"]), Ok(CliCommand::Help));
        assert_eq!(p(&["--version"]), Ok(CliCommand::Version));
        assert_eq!(p(&["-V"]), Ok(CliCommand::Version));
    }

    #[test]
    fn plain_verbs() {
        assert_eq!(p(&["disable"]), Ok(CliCommand::Disable));
        assert_eq!(p(&["status"]), Ok(CliCommand::Status));
        assert_eq!(p(&["devices"]), Ok(CliCommand::Devices));
        assert_eq!(p(&["shutdown"]), Ok(CliCommand::Shutdown));
    }

    #[test]
    fn enable_without_flags_keeps_current_settings() {
        assert_eq!(
            p(&["enable"]),
            Ok(CliCommand::Enable {
                device_id: None,
                threshold: None,
                mic_name: None,
                rumble_filter: None,
                auto_level: None,
            })
        );
    }

    #[test]
    fn enable_flags_in_space_and_equals_forms() {
        let expected = Ok(CliCommand::Enable {
            device_id: Some(5),
            threshold: Some(70),
            mic_name: Some("My-Mic_2".to_string()),
            rumble_filter: Some(false),
            auto_level: Some(true),
        });
        assert_eq!(
            p(&[
                "enable",
                "--device",
                "5",
                "--threshold",
                "70",
                "--mic-name",
                "My-Mic_2",
                "--rumble-filter",
                "off",
                "--auto-level",
                "on"
            ]),
            expected
        );
        assert_eq!(
            p(&[
                "enable",
                "--device=5",
                "--threshold=70",
                "--mic-name=My-Mic_2",
                "--rumble-filter=off",
                "--auto-level=on"
            ]),
            expected
        );
    }

    #[test]
    fn toggle_flags_accept_only_on_or_off() {
        let rumble = |args: &[&str]| match p(args) {
            Ok(CliCommand::Enable { rumble_filter, .. }) => rumble_filter,
            other => panic!("expected Enable, got {other:?}"),
        };
        assert_eq!(rumble(&["enable", "--rumble-filter", "on"]), Some(true));
        assert_eq!(rumble(&["enable", "--rumble-filter", "off"]), Some(false));
        assert!(p(&["enable", "--rumble-filter", "true"]).is_err());
        assert!(p(&["enable", "--rumble-filter", "ON"]).is_err());
        assert!(p(&["enable", "--rumble-filter"]).is_err());
        assert!(p(&["enable", "--rumble-filter", "1"]).is_err());
        assert!(p(&["enable", "--rumble-filter=on", "--rumble-filter=on"]).is_err());
        assert!(p(&["enable", "--auto-level", "yes"]).is_err());
        assert!(p(&["enable", "--auto-level=on", "--auto-level=off"]).is_err());
    }

    #[test]
    fn threshold_bounds_are_inclusive() {
        assert!(p(&["enable", "--threshold", "0"]).is_ok());
        assert!(p(&["enable", "--threshold", "100"]).is_ok());
        assert!(p(&["enable", "--threshold", "101"]).is_err());
        assert!(p(&["enable", "--threshold", "-1"]).is_err());
    }

    #[test]
    fn invalid_values_rejected() {
        assert!(p(&["enable", "--device", "abc"]).is_err());
        assert!(p(&["enable", "--mic-name", "bad name!"]).is_err());
        assert!(p(&["enable", "--mic-name", &"x".repeat(MIC_NAME_MAX_LEN + 1)]).is_err());
    }

    #[test]
    fn duplicate_and_valueless_flags_rejected() {
        assert!(p(&["enable", "--device", "1", "--device", "2"]).is_err());
        assert!(p(&["enable", "--device"]).is_err());
    }

    #[test]
    fn unknown_tokens_rejected() {
        assert!(p(&["frobnicate"]).is_err());
        assert!(p(&["Enable"]).is_err());
        assert!(p(&["enable", "--frob"]).is_err());
        assert!(p(&["enable", "extra"]).is_err());
        assert!(p(&["status", "extra"]).is_err());
    }

    #[test]
    fn non_utf8_argument_is_an_error() {
        use std::os::unix::ffi::OsStringExt;
        assert!(parse([OsString::from_vec(vec![0xFF])]).is_err());
    }
}
