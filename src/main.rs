//  src/main.rs

//! `ostiarius` checks whether a `cargo test` gate proves what it claims.
//!
//! The `count` subcommand asserts how many tests a run passed, in total, per
//! test binary, or by name. The `mutate` subcommand runs declared mutations,
//! each of which must fail the tests it names, and restores every file it edits
//! byte for byte.
//!
//! The crate is a binary rather than a library. Its only interfaces are its
//! command line and its mutation-table format, so no Rust API is published.

mod cli;
mod count;
mod decode;
mod interrupt;
mod json;
mod mutate;
mod reader;
mod split;

use std::process::ExitCode;

const HELP: &str = "\
ostiarius: check that a cargo test gate proves what it claims

Usage:
  ostiarius count [OPTIONS] (LOG... | -- COMMAND...)
  ostiarius mutate [OPTIONS] TABLE...
  ostiarius --help | --version

Commands:
  count    assert how many tests a cargo test run passed: in total,
           per test binary, or by name
  mutate   run declared mutations that must fail named tests, restoring
           every file byte for byte

Run `ostiarius <command> --help` for a command's options.
";

fn main() -> ExitCode {
    let mut args = Vec::new();
    for arg in std::env::args_os().skip(1) {
        match arg.into_string() {
            Ok(arg) => args.push(arg),
            Err(arg) => {
                eprintln!(
                    "error: the argument {} is not valid Unicode",
                    arg.to_string_lossy()
                );
                return ExitCode::from(3);
            }
        }
    }
    match args.first().map(String::as_str) {
        Some("count") => count::run(&args[1..]),
        Some("mutate") => mutate::run(&args[1..]),
        Some("-h" | "--help" | "help") => {
            print!("{HELP}");
            ExitCode::SUCCESS
        }
        Some("-V" | "--version") => {
            println!("ostiarius {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("error: unknown command {other:?}");
            eprint!("{HELP}");
            ExitCode::from(3)
        }
        None => {
            eprint!("{HELP}");
            ExitCode::from(3)
        }
    }
}
