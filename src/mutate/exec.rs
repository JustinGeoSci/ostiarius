//  src/mutate/exec.rs

//! An entry's command is run here, and what it reported is read back from its
//! log.

use std::fs::File;
use std::io;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::kill;

use crate::decode::decode;
use crate::reader::libtest::Libtest;
use crate::reader::{Reader, Run};

// ===========================================================================
// Results
// ===========================================================================

pub(crate) struct CommandResult {
    /// The exit code, or `None` if a signal ended the command.
    pub exit: Option<i32>,
    pub run: Run,
    /// The command exited non-zero without printing a single result line,
    /// meaning the tests never ran, almost always because the build failed.
    pub build_failed: bool,
    /// The output contains the compiler's error text (`could not compile`).
    pub compiler_error: bool,
}

// ===========================================================================
// Arguments
// ===========================================================================

/// The arguments actually run. `cargo test` stops at the first test binary with
/// a failure unless told otherwise, which would hide failures in later
/// binaries, so `--no-fail-fast` is added to a `cargo [+toolchain] test`
/// command that lacks it. The flag can only reveal more failing tests, never
/// fewer, which makes the check stricter rather than looser.
pub(crate) fn effective_args(args: &[String]) -> Vec<String> {
    let mut out = args.to_vec();
    let is_cargo = args
        .first()
        .and_then(|a| Path::new(a).file_stem())
        .is_some_and(|stem| stem.eq_ignore_ascii_case("cargo"));
    if !is_cargo {
        return out;
    }
    let sub = if args.get(1).is_some_and(|a| a.starts_with('+')) {
        2
    } else {
        1
    };
    if args.get(sub).map(String::as_str) != Some("test") {
        return out;
    }
    let before_double_dash = args.iter().take_while(|a| *a != "--");
    if !before_double_dash
        .into_iter()
        .any(|a| a == "--no-fail-fast")
    {
        out.insert(sub + 1, "--no-fail-fast".to_owned());
    }
    out
}

// ===========================================================================
// Running
// ===========================================================================

/// How a command's run ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Ended {
    Exited,
    /// The command outlived its timeout, and its process tree was killed.
    TimedOut,
    /// Ctrl-C was pressed, and the command's process tree was killed.
    Interrupted,
}

/// How often the wait checks the deadline and the Ctrl-C request.
const POLL: Duration = Duration::from_millis(100);

/// Runs `args` in `cwd` with `env` added and writes stdout and stderr to `log`,
/// one file for both so their order is kept, then reads the log.
///
/// The wait polls rather than blocks, so neither a hang nor Ctrl-C can hold it.
/// Past `timeout`, or once `interrupted()` is true, the command's whole process
/// tree is killed before this function returns, so the caller never restores a
/// file underneath a build that is still running. A killed run's partial log is
/// still read, though only for its messages.
pub(crate) fn run_command(
    args: &[String],
    env: &[(String, String)],
    log: &Path,
    cwd: &Path,
    timeout: Option<Duration>,
    interrupted: &dyn Fn() -> bool,
) -> io::Result<(Ended, CommandResult)> {
    let args = effective_args(args);
    let out = File::create(log)?;
    let err = out.try_clone()?;
    let mut command = Command::new(&args[0]);
    command
        .args(&args[1..])
        .current_dir(cwd)
        .envs(env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err);
    in_own_group(&mut command);
    let mut child = command.spawn()?;
    let deadline = timeout.map(|t| Instant::now() + t);
    let (ended, exit) = loop {
        if let Some(status) = child.try_wait()? {
            break (Ended::Exited, status.code());
        }
        if interrupted() {
            kill::kill_tree(&mut child);
            break (Ended::Interrupted, None);
        }
        if deadline.is_some_and(|d| Instant::now() >= d) {
            kill::kill_tree(&mut child);
            break (Ended::TimedOut, None);
        }
        std::thread::sleep(POLL);
    };
    let (text, _) = decode(&std::fs::read(log)?);
    let run = Libtest.parse(&text);
    let result = CommandResult {
        exit,
        build_failed: exit != Some(0) && run.binaries.is_empty(),
        compiler_error: text.contains("could not compile"),
        run,
    };
    Ok((ended, result))
}

/// On Unix, makes the command the leader of a new process group, so that the
/// group is exactly its tree and can be killed as one (see `kill`). It also
/// keeps the terminal's Ctrl-C from reaching the tree directly, because the run
/// kills the tree itself and in order. Windows finds the tree from the parent's
/// id and needs nothing here.
fn in_own_group(command: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(not(unix))]
    let _ = command;
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::{Ended, effective_args, run_command};
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    fn log(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("ostiarius-exec-{name}-{}.log", std::process::id()))
    }

    fn sleeper() -> Vec<String> {
        let args: &[&str] = if cfg!(windows) {
            &["ping", "-n", "30", "127.0.0.1"]
        } else {
            &["sleep", "30"]
        };
        args.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn ended_exited_on_quick_command() {
        let args: Vec<String> = if cfg!(windows) {
            vec!["cmd".into(), "/c".into(), "exit 3".into()]
        } else {
            vec!["sh".into(), "-c".into(), "exit 3".into()]
        };
        let path = log("quick");
        let (ended, result) =
            run_command(&args, &[], &path, &std::env::temp_dir(), None, &|| false).expect("runs");
        let _ = std::fs::remove_file(&path);
        assert_eq!((ended, result.exit), (Ended::Exited, Some(3)));
        assert!(result.build_failed, "no result line and a non-zero exit");
    }

    #[test]
    fn ended_timed_out_on_sleeper() {
        let path = log("sleeper");
        let start = Instant::now();
        let (ended, result) = run_command(
            &sleeper(),
            &[],
            &path,
            &std::env::temp_dir(),
            Some(Duration::from_millis(500)),
            &|| false,
        )
        .expect("runs");
        let _ = std::fs::remove_file(&path);
        assert_eq!((ended, result.exit), (Ended::TimedOut, None));
        assert!(
            start.elapsed() < Duration::from_secs(15),
            "the timeout fired late: {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn interrupt_flag_ends_wait() {
        let path = log("interrupt");
        let start = Instant::now();
        let (ended, _) = run_command(&sleeper(), &[], &path, &std::env::temp_dir(), None, &|| {
            start.elapsed() > Duration::from_millis(300)
        })
        .expect("runs");
        let _ = std::fs::remove_file(&path);
        assert_eq!(ended, Ended::Interrupted);
        assert!(start.elapsed() < Duration::from_secs(15));
    }

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn no_fail_fast_inserted() {
        assert_eq!(
            effective_args(&v(&["cargo", "test", "-p", "x"])),
            v(&["cargo", "test", "--no-fail-fast", "-p", "x"])
        );
    }

    #[test]
    fn no_fail_fast_not_duplicated() {
        let args = v(&["cargo", "test", "-p", "x", "--no-fail-fast"]);
        assert_eq!(effective_args(&args), args);
    }

    #[test]
    fn toolchain_form_handled() {
        assert_eq!(
            effective_args(&v(&["cargo", "+1.90", "test"])),
            v(&["cargo", "+1.90", "test", "--no-fail-fast"])
        );
        assert_eq!(
            effective_args(&v(&["C:/tools/cargo.exe", "test"])),
            v(&["C:/tools/cargo.exe", "test", "--no-fail-fast"])
        );
    }

    #[test]
    fn non_cargo_command_untouched() {
        for args in [
            v(&["cargo", "build"]),
            v(&["sh", "-c", "cargo test"]),
            v(&["cargo-nextest", "run"]),
        ] {
            assert_eq!(effective_args(&args), args);
        }
    }

    #[test]
    fn libtest_flag_after_double_dash_is_not_cargos() {
        assert_eq!(
            effective_args(&v(&["cargo", "test", "--", "--no-fail-fast"])),
            v(&["cargo", "test", "--no-fail-fast", "--", "--no-fail-fast"])
        );
    }
}
