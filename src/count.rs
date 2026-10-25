//  src/count.rs

//! `ostiarius count` asserts what a `cargo test` run passed.
//!
//! It reads saved logs or runs the command itself, then checks the totals,
//! per-binary floors and named tests the user asked for. Matching nothing is a
//! failure unless `--allow-empty` says it is expected. Without that rule a gate
//! whose filter names a module path that no longer exists passes silently,
//! which is the exact failure this command exists to catch.

use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use crate::cli::{self, Opt, UsageError};
use crate::decode::decode;
use crate::json::Value;
use crate::reader::libtest::{Libtest, normalize_name};
use crate::reader::{Binary, Reader, Run};

pub(crate) const HELP: &str = "\
ostiarius count: assert how many tests a cargo test run passed

Usage:
  ostiarius count [OPTIONS] LOG...            read saved logs
  ostiarius count [OPTIONS] -- COMMAND...     run the command and read its output

Options:
  --expect-passed N        exactly N tests must pass in total
  --min-passed N           at least N tests must pass in total
  --min-passed-in LABEL=N  at least N must pass in the test binary LABEL (repeatable);
                           --per-binary prints the labels
  --allow-empty            do not fail when no test passed (by default that fails)
  --expect-names FILE      every test named in FILE (one per line, # comments) must exist
  --list-mode              with --expect-names: list the tests instead of running them
                           (adds `--list --format terse` to the command)
  --log PATH               with a command: also save its output to PATH
  --per-binary             print each test binary's counts
  --json                   print the result as one JSON object
  -h, --help               print this help

Exit codes:
  0  every check passed
  1  a test failed, the command failed, or a count or name check failed
  2  no result could be read, or the log cannot be trusted
  3  usage error

A log must hold both of cargo's streams, because its `Running` headers go to
stderr while the results go to stdout. Logs may be UTF-8 or UTF-16 with a
byte-order mark. In Windows PowerShell 5.1, merge the streams in cmd rather
than with PowerShell's own 2>&1, which rewrites every stderr line, as in
    cmd /c \"cargo test 2>&1\" > test.log
Running the command here (`-- COMMAND`) avoids saving a log at all.

A test's captured output is ignored, so a test that prints a line shaped like
a result changes nothing. For the same reason a command run here may not use
--nocapture or --show-output.

Examples:
  ostiarius count --min-passed 120 target/test.log
  ostiarius count --min-passed 40 -- cargo test -p mycrate --release
  ostiarius count --min-passed-in \"unittests src/lib.rs (mycrate)=30\" -- cargo test
  ostiarius count --expect-names names.txt --list-mode -- cargo test -p mycrate
";

const OPTS: &[Opt] = &[
    Opt::value("expect-passed"),
    Opt::value("min-passed"),
    Opt::repeated("min-passed-in"),
    Opt::flag("allow-empty"),
    Opt::value("expect-names"),
    Opt::flag("list-mode"),
    Opt::value("log"),
    Opt::flag("per-binary"),
    Opt::flag("json"),
    Opt::flag("help"),
];

/// Flags that make libtest print tests' output inline, where it mixes with the
/// result lines this command reads.
pub(crate) const REFUSED_FLAGS: [&str; 2] = ["--nocapture", "--show-output"];

// ===========================================================================
// Options
// ===========================================================================

enum Input {
    Logs(Vec<PathBuf>),
    Command(Vec<String>),
}

// Each flag is an independent command-line switch, and grouping them would
// only rename them.
#[allow(clippy::struct_excessive_bools)]
struct Options {
    expect: Option<u64>,
    min: Option<u64>,
    floors: Vec<(String, u64)>,
    allow_empty: bool,
    names: Option<PathBuf>,
    list_mode: bool,
    log: Option<PathBuf>,
    per_binary: bool,
    json: bool,
    input: Input,
}

fn options(args: &[String]) -> Result<Option<Options>, UsageError> {
    if args.iter().take_while(|a| *a != "--").any(|a| a == "-h") {
        return Ok(None);
    }
    let p = cli::parse(args, OPTS)?;
    if p.flag("help") {
        return Ok(None);
    }
    let expect = p
        .value("expect-passed")
        .map(|v| cli::count_value("expect-passed", v))
        .transpose()?;
    let min = p
        .value("min-passed")
        .map(|v| cli::count_value("min-passed", v))
        .transpose()?;
    if expect.is_some() && min.is_some() {
        return Err(UsageError(
            "give --expect-passed or --min-passed, not both".to_owned(),
        ));
    }
    let mut floors = Vec::new();
    for item in p.values("min-passed-in") {
        let Some((label, n)) = item.rsplit_once('=') else {
            return Err(UsageError(format!(
                "--min-passed-in needs LABEL=N, not {item:?}"
            )));
        };
        floors.push((
            label.trim().to_owned(),
            cli::count_value("min-passed-in", n.trim())?,
        ));
    }
    let list_mode = p.flag("list-mode");
    let names = p.value("expect-names").map(PathBuf::from);
    if list_mode && names.is_none() {
        return Err(UsageError("--list-mode needs --expect-names".to_owned()));
    }
    if list_mode && (expect.is_some() || min.is_some() || !floors.is_empty()) {
        return Err(UsageError(
            "--list-mode runs no tests, so it cannot check counts".to_owned(),
        ));
    }
    let log = p.value("log").map(PathBuf::from);
    let (allow_empty, per_binary, json) =
        (p.flag("allow-empty"), p.flag("per-binary"), p.flag("json"));
    let input = match (p.command, p.positional.is_empty()) {
        (Some(_), false) => {
            return Err(UsageError(
                "give LOG files or `-- COMMAND`, not both".to_owned(),
            ));
        }
        (Some(cmd), true) if cmd.is_empty() => {
            return Err(UsageError("`--` must be followed by a command".to_owned()));
        }
        (Some(cmd), true) => Input::Command(cmd),
        (None, false) => Input::Logs(p.positional.iter().map(PathBuf::from).collect()),
        (None, true) => {
            return Err(UsageError(
                "give LOG files or `-- COMMAND` (see --help)".to_owned(),
            ));
        }
    };
    if let Input::Command(cmd) = &input {
        if let Some(flag) = cmd.iter().find(|a| REFUSED_FLAGS.contains(&a.as_str())) {
            return Err(UsageError(format!(
                "the command uses {flag}, which prints tests' output among the result lines this command reads"
            )));
        }
    } else if list_mode {
        return Err(UsageError("--list-mode needs `-- COMMAND`".to_owned()));
    }
    if log.is_some() && matches!(input, Input::Logs(_)) {
        return Err(UsageError(
            "--log is for a command's output; with LOG files it does nothing".to_owned(),
        ));
    }
    Ok(Some(Options {
        expect,
        min,
        floors,
        allow_empty,
        names,
        list_mode,
        log,
        per_binary,
        json,
        input,
    }))
}

/// Turns `cargo test -p x` into `cargo test -p x -- --list --format terse`.
/// When the command already has a `--`, the listing flags go after it.
fn list_command(cmd: &[String]) -> Vec<String> {
    let mut out = cmd.to_vec();
    if !out.iter().any(|a| a == "--") {
        out.push("--".to_owned());
    }
    out.extend(["--list", "--format", "terse"].map(String::from));
    out
}

// ===========================================================================
// Gathering output
// ===========================================================================

/// How the command that produced the output ended.
enum Ended {
    /// The output came from saved logs, so no command ran.
    NotRun,
    Code(i32),
    /// A signal ended the command, so it has no exit code.
    Signal,
}

struct Gathered {
    text: String,
    lossy: bool,
    ended: Ended,
}

fn gather(opts: &Options) -> Result<Gathered, String> {
    match &opts.input {
        Input::Logs(paths) => {
            let mut text = String::new();
            let mut lossy = false;
            for path in paths {
                let bytes = std::fs::read(path)
                    .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
                let (part, part_lossy) = decode(&bytes);
                text.push_str(&part);
                if !text.ends_with('\n') {
                    text.push('\n');
                }
                lossy |= part_lossy;
            }
            Ok(Gathered {
                text,
                lossy,
                ended: Ended::NotRun,
            })
        }
        Input::Command(cmd) => {
            let cmd = if opts.list_mode {
                list_command(cmd)
            } else {
                cmd.clone()
            };
            let (bytes, code) = run_command(&cmd, opts.log.as_deref(), opts.json)
                .map_err(|e| format!("cannot run {}: {e}", cmd.join(" ")))?;
            let (text, lossy) = decode(&bytes);
            let ended = code.map_or(Ended::Signal, Ended::Code);
            Ok(Gathered { text, lossy, ended })
        }
    }
}

/// Runs `cmd` with stdout and stderr on one pipe, echoing the output as it
/// arrives.
///
/// One pipe is used rather than two because Cargo prints each binary's
/// `Running` header on stderr while libtest prints its results on stdout, and
/// only a single pipe keeps the two in the order they were written. The
/// per-binary labels depend on that order. Under `--json` the echo goes to
/// stderr, so stdout holds nothing but the JSON.
fn run_command(
    cmd: &[String],
    log: Option<&Path>,
    echo_to_stderr: bool,
) -> io::Result<(Vec<u8>, Option<i32>)> {
    let (mut reader, writer) = io::pipe()?;
    let writer_for_stderr = writer.try_clone()?;
    let mut log_file = log.map(File::create).transpose()?;
    let mut child = {
        let mut command = Command::new(&cmd[0]);
        command
            .args(&cmd[1..])
            .stdin(Stdio::null())
            .stdout(writer)
            .stderr(writer_for_stderr);
        command.spawn()?
        // `command` is dropped here, closing this process's copies of the
        // write end, so the reads below end when the child's copies close.
    };
    let mut collected = Vec::new();
    let mut buf = [0_u8; 8192];
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        let chunk = &buf[..n];
        // Echo failures (a closed console) must not stop the run.
        if echo_to_stderr {
            let _ = io::stderr().write_all(chunk);
        } else {
            let _ = io::stdout().write_all(chunk);
        }
        if let Some(f) = log_file.as_mut() {
            f.write_all(chunk)?;
        }
        collected.extend_from_slice(chunk);
    }
    let status = child.wait()?;
    Ok((collected, status.code()))
}

// ===========================================================================
// Checking
// ===========================================================================

#[derive(Default)]
struct Report {
    run: Run,
    listed: Option<Vec<String>>,
    /// Checks that failed, each of which makes the exit code 1.
    problems: Vec<String>,
    missing_names: Vec<String>,
    /// The reason no result could be trusted, which makes the exit code 2.
    fatal: Option<String>,
    lossy: bool,
}

fn check(opts: &Options, gathered: &Gathered) -> Report {
    let mut report = Report {
        lossy: gathered.lossy,
        ..Report::default()
    };
    let exit_note = match gathered.ended {
        Ended::NotRun | Ended::Code(0) => String::new(),
        Ended::Code(code) => format!(" (the command exited with code {code})"),
        Ended::Signal => " (the command was ended by a signal)".to_owned(),
    };
    if opts.list_mode {
        let names = Libtest.parse_list(&gathered.text);
        if names.is_empty() {
            report.fatal = Some(format!("no tests were listed{exit_note}"));
        } else if !exit_note.is_empty() {
            report.problems.push(format!("listing failed{exit_note}"));
        }
        report.listed = Some(names);
    } else {
        report.run = Libtest.parse(&gathered.text);
        if let Some(reason) = &report.run.suspect {
            report.fatal = Some(format!("the log cannot be trusted: {reason}"));
            return report;
        }
        if report.run.binaries.is_empty() {
            report.fatal = Some(format!("no \"test result:\" line found{exit_note}"));
            return report;
        }
        check_counts(opts, &mut report, &exit_note);
    }
    if let Some(path) = &opts.names {
        check_names(path, &mut report);
    }
    report
}

fn check_counts(opts: &Options, report: &mut Report, exit_note: &str) {
    let failing = report.run.failing();
    if !failing.is_empty() {
        report
            .problems
            .push(format!("tests failed: {}", failing.join(", ")));
    } else if report.run.failed() > 0 {
        report
            .problems
            .push(format!("{} tests failed", report.run.failed()));
    }
    if !exit_note.is_empty() {
        report
            .problems
            .push(format!("the command failed{exit_note}"));
    }
    let passed = report.run.passed();
    if passed == 0 && !opts.allow_empty {
        report
            .problems
            .push("no test passed (does the filter match anything?); use --allow-empty if that is expected".to_owned());
    }
    if let Some(n) = opts.expect {
        if passed != n {
            report
                .problems
                .push(format!("expected exactly {n} passed, got {passed}"));
        }
    }
    if let Some(n) = opts.min {
        if passed < n {
            report
                .problems
                .push(format!("expected at least {n} passed, got {passed}"));
        }
    }
    for (label, n) in &opts.floors {
        match resolve_label(label, &report.run.binaries) {
            Ok(b) if b.passed < *n => {
                report.problems.push(format!(
                    "{}: expected at least {n} passed, got {}",
                    b.label, b.passed
                ));
            }
            Ok(_) => {}
            Err(why) => report.problems.push(why),
        }
    }
}

/// Finds the one binary a `--min-passed-in` label names, either by its full
/// label or by its target text alone (`unittests src/lib.rs`) when only one
/// binary has that text.
fn resolve_label<'a>(label: &str, binaries: &'a [Binary]) -> Result<&'a Binary, String> {
    let full: Vec<&Binary> = binaries.iter().filter(|b| b.label == label).collect();
    let matches = if full.is_empty() {
        binaries
            .iter()
            .filter(|b| target_text(&b.label) == label)
            .collect()
    } else {
        full
    };
    match matches.as_slice() {
        [one] => Ok(one),
        [] => Err(format!(
            "no test binary matches the label {label:?} (the binaries are: {})",
            binaries
                .iter()
                .map(|b| b.label.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        )),
        several => Err(format!(
            "the label {label:?} matches {} test binaries ({}); give the full label",
            several.len(),
            several
                .iter()
                .map(|b| b.label.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        )),
    }
}

/// Reduces `unittests src/lib.rs (demo)` to `unittests src/lib.rs`.
fn target_text(label: &str) -> &str {
    label.rsplit_once(" (").map_or(label, |(target, _)| target)
}

fn check_names(path: &Path, report: &mut Report) {
    let wanted = match std::fs::read(path) {
        Ok(bytes) => decode(&bytes).0,
        Err(e) => {
            report.fatal = Some(format!("cannot read {}: {e}", path.display()));
            return;
        }
    };
    let present: Vec<&str> = match &report.listed {
        Some(listed) => listed.iter().map(String::as_str).collect(),
        None => report
            .run
            .binaries
            .iter()
            .flat_map(|b| b.tests.iter())
            .map(|(name, _)| name.as_str())
            .collect(),
    };
    for line in wanted.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let name = normalize_name(line);
        if !present.contains(&name.as_str()) && !report.missing_names.contains(&name) {
            report.missing_names.push(name);
        }
    }
    if !report.missing_names.is_empty() {
        report.problems.push(format!(
            "tests not found: {}",
            report.missing_names.join(", ")
        ));
    }
}

// ===========================================================================
// Output
// ===========================================================================

pub(crate) fn run(args: &[String]) -> ExitCode {
    let opts = match options(args) {
        Ok(Some(opts)) => opts,
        Ok(None) => {
            print!("{HELP}");
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("error: {e}");
            eprintln!("(see ostiarius count --help)");
            return ExitCode::from(3);
        }
    };
    let report = match gather(&opts) {
        Ok(gathered) => check(&opts, &gathered),
        Err(fatal) => Report {
            fatal: Some(fatal),
            ..Report::default()
        },
    };
    let code = if report.fatal.is_some() {
        2
    } else {
        u8::from(!report.problems.is_empty())
    };
    if opts.json {
        println!("{}", to_json(&report, code).render());
    } else {
        print_text(&opts, &report);
    }
    ExitCode::from(code)
}

fn print_text(opts: &Options, report: &Report) {
    if report.lossy {
        eprintln!("warning: the output was not valid text; unreadable bytes were replaced");
    }
    if let Some(fatal) = &report.fatal {
        eprintln!("error: {fatal}");
        return;
    }
    if let Some(listed) = &report.listed {
        println!("{} tests listed", listed.len());
    } else {
        let run = &report.run;
        println!(
            "{} passed, {} failed, {} ignored, in {} test binaries",
            run.passed(),
            run.failed(),
            run.ignored(),
            run.binaries.len()
        );
        if opts.per_binary {
            for b in &run.binaries {
                println!(
                    "  {}: {} passed, {} failed, {} ignored",
                    b.label, b.passed, b.failed, b.ignored
                );
            }
        }
    }
    for problem in &report.problems {
        println!("FAILED: {problem}");
    }
}

fn to_json(report: &Report, code: u8) -> Value {
    let binaries = report
        .run
        .binaries
        .iter()
        .map(|b| {
            Value::Obj(vec![
                ("label", Value::str(b.label.clone())),
                ("passed", Value::Num(b.passed)),
                ("failed", Value::Num(b.failed)),
                ("ignored", Value::Num(b.ignored)),
            ])
        })
        .collect();
    let mut fields = vec![
        ("ok", Value::Bool(code == 0)),
        ("exit_code", Value::Num(u64::from(code))),
        ("passed", Value::Num(report.run.passed())),
        ("failed", Value::Num(report.run.failed())),
        ("ignored", Value::Num(report.run.ignored())),
        ("binaries", Value::Arr(binaries)),
        ("failing", Value::strs(report.run.failing())),
        (
            "missing_names",
            Value::strs(report.missing_names.iter().cloned()),
        ),
        ("problems", Value::strs(report.problems.iter().cloned())),
        ("lossy", Value::Bool(report.lossy)),
    ];
    if let Some(listed) = &report.listed {
        fields.push(("listed", Value::Num(listed.len() as u64)));
    }
    if let Some(fatal) = &report.fatal {
        fields.push(("error", Value::str(fatal.clone())));
    }
    Value::Obj(fields)
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::{Binary, list_command, options, resolve_label};

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    fn binary(label: &str, passed: u64) -> Binary {
        Binary {
            label: label.to_owned(),
            passed,
            failed: 0,
            ignored: 0,
            tests: Vec::new(),
        }
    }

    fn binaries() -> Vec<Binary> {
        vec![
            binary("unittests src/lib.rs (demo)", 3),
            binary("unittests src/lib.rs (other)", 4),
            binary("tests/basic.rs (basic)", 2),
        ]
    }

    #[test]
    fn label_full_match() {
        let b = binaries();
        assert_eq!(
            resolve_label("unittests src/lib.rs (other)", &b).map(|b| b.passed),
            Ok(4)
        );
    }

    #[test]
    fn label_unique_target_match() {
        let b = binaries();
        assert_eq!(resolve_label("tests/basic.rs", &b).map(|b| b.passed), Ok(2));
    }

    #[test]
    fn label_ambiguous_is_error() {
        let b = binaries();
        let err = resolve_label("unittests src/lib.rs", &b)
            .map(|_| ())
            .expect_err("ambiguous");
        assert!(err.contains("matches 2 test binaries"), "{err}");
    }

    #[test]
    fn label_absent_is_error() {
        let b = binaries();
        let err = resolve_label("tests/gone.rs", &b)
            .map(|_| ())
            .expect_err("absent");
        assert!(err.contains("no test binary matches"), "{err}");
    }

    #[test]
    fn list_mode_inserts_after_existing_double_dash() {
        assert_eq!(
            list_command(&args(&["cargo", "test", "--", "--test-threads", "1"])),
            args(&[
                "cargo",
                "test",
                "--",
                "--test-threads",
                "1",
                "--list",
                "--format",
                "terse"
            ])
        );
    }

    #[test]
    fn list_mode_adds_double_dash() {
        assert_eq!(
            list_command(&args(&["cargo", "test", "-p", "demo"])),
            args(&[
                "cargo", "test", "-p", "demo", "--", "--list", "--format", "terse"
            ])
        );
    }

    #[test]
    fn nocapture_refused() {
        let err = options(&args(&["--", "cargo", "test", "--", "--nocapture"]))
            .map(|_| ())
            .expect_err("refused");
        assert!(err.0.contains("--nocapture"), "{err}");
    }

    #[test]
    fn expect_and_min_exclusive() {
        assert!(
            options(&args(&[
                "--expect-passed",
                "1",
                "--min-passed",
                "1",
                "x.log"
            ]))
            .is_err()
        );
    }

    #[test]
    fn list_mode_needs_names_and_a_command() {
        assert!(options(&args(&["--list-mode", "--", "cargo", "test"])).is_err());
        assert!(options(&args(&["--list-mode", "--expect-names", "n.txt", "x.log"])).is_err());
        assert!(
            options(&args(&[
                "--list-mode",
                "--expect-names",
                "n.txt",
                "--min-passed",
                "1",
                "--",
                "c"
            ]))
            .is_err()
        );
    }

    #[test]
    fn floor_needs_label_and_count() {
        assert!(options(&args(&["--min-passed-in", "nolabel", "x.log"])).is_err());
        assert!(options(&args(&["--min-passed-in", "a=b=3", "x.log"])).is_ok());
    }

    #[test]
    fn logs_and_command_exclusive() {
        assert!(options(&args(&["x.log", "--", "cargo", "test"])).is_err());
        assert!(options(&args(&[])).is_err());
    }
}
