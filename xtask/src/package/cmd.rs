//! Running the outside programs `package` drives.

use std::process::{Command, Stdio};

fn describe(command: &Command) -> String {
    std::iter::once(command.get_program())
        .chain(command.get_args())
        .map(|part| part.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Runs `command` with its output on the terminal; a non-zero exit is an
/// error.
pub fn run(command: &mut Command) -> Result<(), String> {
    let status = command
        .status()
        .map_err(|e| format!("{}: {e}", describe(command)))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("`{}` failed: {status}", describe(command)))
    }
}

/// Runs `command` and returns its standard output; a non-zero exit is an
/// error.
pub fn output(command: &mut Command) -> Result<String, String> {
    let output = command
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| format!("{}: {e}", describe(command)))?;
    if !output.status.success() {
        return Err(format!("`{}` failed: {}", describe(command), output.status));
    }
    String::from_utf8(output.stdout).map_err(|e| format!("{}: {e}", describe(command)))
}

/// Runs `command` and returns its exit code with standard output and error
/// together, for a check that judges the failure instead of stopping on it.
pub fn capture(command: &mut Command) -> Result<(Option<i32>, String), String> {
    let output = command
        .output()
        .map_err(|e| format!("{}: {e}", describe(command)))?;
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    Ok((output.status.code(), text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_is_described_by_its_program_and_arguments() {
        let mut command = Command::new("git");
        command.args(["archive", "HEAD"]);
        assert_eq!(describe(&command), "git archive HEAD");
    }
}
