//! Packaging tasks for Spectral, run as `cargo xtask <task>`.
//!
//! `install` is the one list of the files a Spectral installation consists
//! of; every package built from this repository stages its files through
//! it. `package` builds a format's package from a git ref and checks it.

mod install;
mod package;
mod workspace;

use std::ffi::OsString;
use std::process::ExitCode;

const USAGE: &str =
    "usage: cargo xtask install --prefix <dir> [--destdir <dir>] [--licensedir <dir>] [--app-only]
                           [--binaries <dir>]
       cargo xtask package <format> [--ref <git-ref>]";

fn main() -> ExitCode {
    // OS strings, since a packager's staging path need not be UTF-8.
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let result = match args.split_first() {
        Some((task, rest)) if task.to_str() == Some("install") => {
            install::Options::parse(rest).and_then(|options| install::run(&options))
        }
        Some((task, rest)) if task.to_str() == Some("package") => {
            package::Options::parse(rest).and_then(|options| package::run(&options))
        }
        _ => Err(USAGE.to_string()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("xtask: {message}");
            ExitCode::FAILURE
        }
    }
}
