//  src/mutate/mod.rs

//! `ostiarius mutate` runs declared mutations, each of which must fail the
//! tests it names, and restores every file it edits byte for byte.
//!
//! A run first loads and validates its tables and refuses a tree that may
//! already hold a mutation (`guard`). It then checks that every edit applies,
//! and runs each distinct command once on the unmutated tree (`baseline`),
//! which must pass and which sets the default timeouts. Each entry then saves
//! its file, edits it, runs the command, and restores the file and confirms its
//! bytes whatever happened in between, whether the tests failed, the command
//! timed out or Ctrl-C was pressed. Only then is the entry judged. Last, each
//! distinct command runs once more on the restored tree.

pub(crate) mod baseline;
pub(crate) mod edit;
pub(crate) mod exec;
pub(crate) mod guard;
pub(crate) mod kill;
pub(crate) mod store;
pub(crate) mod table;
pub(crate) mod verdict;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use crate::cli::{self, Opt, UsageError};
use crate::interrupt;
use baseline::{Baseline, BaselineError};
use exec::Ended;
use table::Entry;

pub(crate) const HELP: &str = "\
ostiarius mutate: run declared mutations that must fail named tests

Usage:
  ostiarius mutate [OPTIONS] TABLE...      run the tables' entries
  ostiarius mutate --restore [--log-dir DIR]

Each TABLE is a mutation table in TOML, format version 1. Each distinct
command first runs once on the unmutated tree, which must pass. For every
entry the file is then saved and edited, the entry's command runs, and the
file is restored byte for byte. The entry passes only if the mutated tree
built, the command failed, every test in `must_fail` failed, and every other
failing test is listed in `may_fail`. Each distinct command then runs once
more on the restored tree and must pass with at least `min_passed` tests.

A run refuses to start, and touches nothing, if an earlier run left saved
copies behind (see --restore), if a target file has uncommitted changes in a
git work tree, or if the unmutated tree already fails. Ctrl-C stops the
current command, restores the file, and exits 130.

Options:
  --id ID          run only this entry (repeatable)
  --list           list the entries and exit
  --dry-run        check that every edit applies, then exit without running
  --root DIR       use DIR as every table's root
  --log-dir DIR    where logs and saved copies go (default target/ostiarius)
  --timeout S      seconds per command run, for entries whose table sets none
                   (by default: 5 times the baseline's time, at least 60 s)
  --skip-baseline  do not run the baseline (every entry then needs a timeout)
  --allow-dirty    run even if target files have uncommitted changes
  --restore        put back every file a stopped run left mutated, and exit
  -h, --help       print this help

Exit codes:
  0    every entry and every restored run passed
  1    an entry or a restored run failed or timed out
  2    refused, a table or an edit has a problem, or a file could not be restored
  3    usage error
  130  interrupted by Ctrl-C (the file being mutated was restored)
";

const OPTS: &[Opt] = &[
    Opt::repeated("id"),
    Opt::flag("list"),
    Opt::flag("dry-run"),
    Opt::value("root"),
    Opt::value("log-dir"),
    Opt::value("timeout"),
    Opt::flag("skip-baseline"),
    Opt::flag("allow-dirty"),
    Opt::flag("restore"),
    Opt::flag("help"),
];

const DEFAULT_LOG_DIR: &str = "target/ostiarius";

// ===========================================================================
// Options
// ===========================================================================

// Each flag is an independent command-line switch.
#[allow(clippy::struct_excessive_bools)]
struct Options {
    tables: Vec<PathBuf>,
    ids: Vec<String>,
    list: bool,
    dry_run: bool,
    root: Option<PathBuf>,
    log_dir: PathBuf,
    timeout: Option<Duration>,
    skip_baseline: bool,
    allow_dirty: bool,
    restore: bool,
}

fn options(args: &[String]) -> Result<Option<Options>, UsageError> {
    if args.iter().any(|a| a == "-h") {
        return Ok(None);
    }
    let p = cli::parse(args, OPTS)?;
    if p.flag("help") {
        return Ok(None);
    }
    if let Some(cmd) = &p.command {
        return Err(UsageError(format!(
            "unexpected `-- {}`: a table's commands are set in the table",
            cmd.join(" ")
        )));
    }
    let restore = p.flag("restore");
    let tables: Vec<PathBuf> = p.positional.iter().map(PathBuf::from).collect();
    let run_options = ["list", "dry-run", "skip-baseline", "allow-dirty"]
        .iter()
        .any(|f| p.flag(f))
        || !p.values("id").is_empty()
        || p.value("timeout").is_some()
        || p.value("root").is_some();
    if restore && (!tables.is_empty() || run_options) {
        return Err(UsageError(
            "--restore takes no tables and no other options except --log-dir".to_owned(),
        ));
    }
    if !restore && tables.is_empty() {
        return Err(UsageError(
            "give at least one TABLE (see --help)".to_owned(),
        ));
    }
    let timeout = match p.value("timeout") {
        None => None,
        Some(v) => Some(
            v.parse::<f64>()
                .ok()
                .filter(|s| s.is_finite() && *s > 0.0)
                .and_then(|s| Duration::try_from_secs_f64(s).ok())
                .ok_or_else(|| {
                    UsageError(format!(
                        "--timeout needs a positive number of seconds, not {v:?}"
                    ))
                })?,
        ),
    };
    let log_dir = PathBuf::from(p.value("log-dir").unwrap_or(DEFAULT_LOG_DIR));
    Ok(Some(Options {
        tables,
        ids: p.values("id").to_vec(),
        list: p.flag("list"),
        dry_run: p.flag("dry-run"),
        root: p.value("root").map(PathBuf::from),
        log_dir: std::path::absolute(&log_dir).unwrap_or(log_dir),
        timeout,
        skip_baseline: p.flag("skip-baseline"),
        allow_dirty: p.flag("allow-dirty"),
        restore,
    }))
}

// ===========================================================================
// The run
// ===========================================================================

/// Why a run stopped before reaching its end.
enum Stop {
    /// A file could not be saved or restored, or the run was refused, so the
    /// exit code is 2.
    Fatal(String),
    /// Ctrl-C was pressed, so the exit code is 130, after the current file was
    /// restored.
    Interrupted(String),
}

pub(crate) fn run(args: &[String]) -> ExitCode {
    let opts = match options(args) {
        Ok(Some(opts)) => opts,
        Ok(None) => {
            print!("{HELP}");
            return ExitCode::SUCCESS;
        }
        Err(e) => return usage(&e.0),
    };
    let keep = opts.log_dir.join(".originals");
    if opts.restore {
        return restore(&keep);
    }
    let entries = match table::load(&opts.tables, opts.root.as_deref()) {
        Ok(entries) => entries,
        Err(errors) => {
            for e in errors {
                eprintln!("error: {e}");
            }
            return ExitCode::from(2);
        }
    };
    let entries = match select(entries, &opts.ids) {
        Ok(entries) => entries,
        Err(e) => return usage(&e),
    };
    if opts.list {
        for e in &entries {
            println!(
                "{:<24} {:<40} {}",
                e.id,
                e.root.join(&e.file).display(),
                e.what
            );
        }
        return ExitCode::SUCCESS;
    }
    if opts.dry_run {
        return dry_run(&entries, &keep, opts.allow_dirty);
    }
    if opts.skip_baseline {
        let missing = baseline::missing_timeouts(&entries, opts.timeout);
        if !missing.is_empty() {
            return usage(&format!(
                "--skip-baseline leaves no baseline to derive a timeout from; give --timeout, or set `timeout` for {missing:?}"
            ));
        }
    }
    // The refusals come before the edit check. A mutation that a stopped run
    // left in place makes its entry's find look stale, and the edit check
    // would then name the wrong cause.
    if let Err(refusal) = refusals(&entries, &keep, opts.allow_dirty) {
        eprintln!("error: {refusal}");
        return ExitCode::from(2);
    }
    if !report_edit_problems(&entries) {
        return ExitCode::from(2);
    }
    if let Err(e) = interrupt::install() {
        eprintln!(
            "error: cannot install the Ctrl-C handler ({e}), so Ctrl-C could not restore files; nothing was run"
        );
        return ExitCode::from(2);
    }
    let outcome = run_all(&entries, &opts, &keep);
    // The store is removed only if it holds no saved copy. A copy kept
    // because a file could not be restored stays behind for --restore.
    let _ = store::remove_if_empty(&keep);
    match outcome {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(Stop::Fatal(msg)) => {
            eprintln!("error: {msg}");
            ExitCode::from(2)
        }
        Err(Stop::Interrupted(msg)) => {
            println!("{msg}");
            ExitCode::from(130)
        }
    }
}

fn usage(msg: &str) -> ExitCode {
    eprintln!("error: {msg}");
    eprintln!("(see ostiarius mutate --help)");
    ExitCode::from(3)
}

/// Reports the refusals as warnings and then checks every edit, without running
/// anything.
fn dry_run(entries: &[Entry], keep: &Path, allow_dirty: bool) -> ExitCode {
    if let Ok(Some(msg)) = guard::leftovers(keep) {
        eprintln!("warning: a real run would refuse: {msg}");
    }
    let (notes, refusal) = guard::dirty(entries);
    for note in notes {
        eprintln!("note: {note}");
    }
    if let (Some(msg), false) = (refusal, allow_dirty) {
        eprintln!("warning: a real run would refuse: {msg}");
    }
    if !report_edit_problems(entries) {
        return ExitCode::from(2);
    }
    println!("ok   dry run: {} entries apply", entries.len());
    ExitCode::SUCCESS
}

/// Prints every edit problem and returns whether there were none.
fn report_edit_problems(entries: &[Entry]) -> bool {
    let problems = check_edits(entries);
    for p in &problems {
        eprintln!("error: {p}");
    }
    problems.is_empty()
}

/// The checks made before the baseline, for saved copies a stopped run left
/// behind and for uncommitted changes in target files.
fn refusals(entries: &[Entry], keep: &Path, allow_dirty: bool) -> Result<(), String> {
    match guard::leftovers(keep) {
        Ok(Some(msg)) => return Err(msg),
        Ok(None) => {}
        Err(e) => return Err(format!("cannot read {}: {e}", keep.display())),
    }
    if allow_dirty {
        return Ok(());
    }
    let (notes, refusal) = guard::dirty(entries);
    for note in notes {
        eprintln!("note: {note}");
    }
    refusal.map_or(Ok(()), Err)
}

fn restore(keep: &Path) -> ExitCode {
    if !keep.is_dir() {
        println!("nothing to restore");
        return ExitCode::SUCCESS;
    }
    match store::restore_all(keep) {
        Ok(restored) if restored.is_empty() => {
            println!("nothing to restore");
            ExitCode::SUCCESS
        }
        Ok(restored) => {
            for path in restored {
                println!("restored {}", path.display());
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(2)
        }
    }
}

fn select(entries: Vec<Entry>, ids: &[String]) -> Result<Vec<Entry>, String> {
    if ids.is_empty() {
        return Ok(entries);
    }
    let unknown: Vec<&String> = ids
        .iter()
        .filter(|id| !entries.iter().any(|e| &e.id == *id))
        .collect();
    if !unknown.is_empty() {
        return Err(format!("no entry has the id {unknown:?}"));
    }
    Ok(entries
        .into_iter()
        .filter(|e| ids.contains(&e.id))
        .collect())
}

/// Reads a file as the bytes it holds and their UTF-8 text.
fn read_text(path: &Path) -> Result<(Vec<u8>, String), String> {
    let bytes = fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let text = String::from_utf8(bytes.clone())
        .map_err(|_| format!("{} is not UTF-8 text", path.display()))?;
    Ok((bytes, text))
}

/// Checks every entry's edit on the bytes it would be applied to, before any
/// file is touched. Every problem is reported rather than only the first.
fn check_edits(entries: &[Entry]) -> Vec<String> {
    let mut problems = Vec::new();
    for e in entries {
        let shown = e.file.display().to_string();
        match read_text(&e.root.join(&e.file)) {
            Ok((_, text)) => {
                if let Err(why) = edit::apply_edit(&text, &e.finds, &e.replaces) {
                    problems.push(format!(
                        "{}: {}: {}",
                        e.table.display(),
                        e.id,
                        why.describe(&shown)
                    ));
                }
            }
            Err(why) => problems.push(format!("{}: {}: {why}", e.table.display(), e.id)),
        }
    }
    problems
}

/// Runs the baselines, the entries and the restored runs.
fn run_all(entries: &[Entry], opts: &Options, keep: &Path) -> Result<bool, Stop> {
    let log_dir = &opts.log_dir;
    fs::create_dir_all(log_dir)
        .map_err(|e| Stop::Fatal(format!("cannot create {}: {e}", log_dir.display())))?;
    let mut roots: Vec<&Path> = Vec::new();
    for e in entries {
        if !roots.contains(&e.root.as_path()) {
            roots.push(&e.root);
        }
    }
    for root in roots {
        println!("root: {}", root.display());
    }
    let groups = baseline::groups(entries);
    let timeouts = baselines(entries, &groups, opts)?;
    let mut ok = true;
    for (entry, timeout) in entries.iter().zip(&timeouts) {
        if interrupt::requested() {
            return Err(Stop::Interrupted(
                "interrupted between entries; no file is mutated".to_owned(),
            ));
        }
        ok &= run_entry(entry, *timeout, log_dir, keep)?;
    }
    store::remove_if_empty(keep)
        .map_err(|e| Stop::Fatal(format!("cannot tidy {}: {e}", keep.display())))?;
    ok &= restored_runs(entries, &groups, &timeouts, log_dir)?;
    Ok(ok)
}

/// Runs each group's baseline unless it is skipped, and returns every entry's
/// timeout, which is either its explicit timeout or one derived from its
/// group's baseline.
fn baselines(
    entries: &[Entry],
    groups: &[Vec<usize>],
    opts: &Options,
) -> Result<Vec<Duration>, Stop> {
    let mut timeouts: Vec<Option<Duration>> = entries
        .iter()
        .map(|e| baseline::explicit(e, opts.timeout))
        .collect();
    if opts.skip_baseline {
        println!("note: the baseline was skipped (--skip-baseline)");
    } else {
        for (k, group) in groups.iter().enumerate() {
            let first = &entries[group[0]];
            let log = opts.log_dir.join(format!("baseline-{}.log", k + 1));
            let Baseline { wall, passed } = match baseline::run(entries, group, &log, opts.timeout)
            {
                Ok(b) => b,
                Err(BaselineError::Interrupted) => {
                    return Err(Stop::Interrupted(
                        "interrupted during the baseline; no file was mutated".to_owned(),
                    ));
                }
                Err(BaselineError::Fails(why)) => {
                    return Err(Stop::Fatal(format!("{why}. Nothing was mutated.")));
                }
            };
            for &i in group {
                timeouts[i] = timeouts[i].or_else(|| Some(baseline::derived(&entries[i], wall)));
            }
            let longest = group
                .iter()
                .filter_map(|&i| timeouts[i])
                .max()
                .unwrap_or_default();
            println!(
                "ok   baseline: {passed} passed in {} (entry timeout up to {}): {}",
                baseline::secs(wall),
                baseline::secs(longest),
                first.command.join(" ")
            );
        }
    }
    // Every entry has a timeout by this point, since --skip-baseline checked
    // for one and the baseline otherwise derived one.
    Ok(timeouts
        .into_iter()
        .map(Option::unwrap_or_default)
        .collect())
}

// ===========================================================================
// One entry
// ===========================================================================

/// Puts a file's original bytes back if it is dropped without being disarmed,
/// so a panic still restores the file.
struct RestoreGuard<'a> {
    path: &'a Path,
    original: &'a [u8],
    armed: bool,
}

impl Drop for RestoreGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _ = store::write_now(self.path, self.original);
        }
    }
}

/// Saves, edits, runs, restores and judges one entry. An `Err` stops the run,
/// either because a file could not be saved or restored or because Ctrl-C was
/// pressed, in which case the file has already been restored.
fn run_entry(entry: &Entry, timeout: Duration, log_dir: &Path, keep: &Path) -> Result<bool, Stop> {
    let path = entry.root.join(&entry.file);
    let label = verdict::label(entry);
    let (original, text) = read_text(&path).map_err(Stop::Fatal)?;
    // Checked again on the bytes about to be edited, in case the file
    // changed since the check before the run.
    let edited = match edit::apply_edit(&text, &entry.finds, &entry.replaces) {
        Ok(edited) => edited,
        Err(why) => {
            println!(
                "FAIL {label}: {}",
                why.describe(&entry.file.display().to_string())
            );
            return Ok(false);
        }
    };
    let saved = store::save(keep, &entry.root, &entry.file, &original)
        .map_err(|e| Stop::Fatal(format!("cannot save a copy of {}: {e}", path.display())))?;
    let log = log_dir.join(format!("{}.log", entry.id));
    let mut guard = RestoreGuard {
        path: &path,
        original: &original,
        armed: true,
    };
    let result = store::write_now(&path, edited.as_bytes()).and_then(|()| {
        exec::run_command(
            &entry.command,
            &entry.env,
            &log,
            &entry.root,
            Some(timeout),
            &interrupt::requested,
        )
    });
    // The command's whole tree is gone by now, whatever ended it.
    restore_one(&path, &original, &saved).map_err(Stop::Fatal)?;
    guard.armed = false;
    store::forget(keep, &saved).map_err(|e| {
        Stop::Fatal(format!(
            "cannot remove the saved copy {}: {e}",
            saved.display()
        ))
    })?;
    match result {
        Ok((Ended::Exited, result)) => {
            let (passed, line) = verdict::judge(entry, &result, &log);
            println!("{line}");
            Ok(passed)
        }
        Ok((Ended::TimedOut, _)) => {
            println!(
                "TIMEOUT {label}: no result after {}; the process tree was killed and the file restored (see {})",
                baseline::secs(timeout),
                log.display()
            );
            Ok(false)
        }
        Ok((Ended::Interrupted, _)) => Err(Stop::Interrupted(format!(
            "interrupted: {} restored byte for byte",
            path.display()
        ))),
        Err(e) => {
            println!(
                "FAIL {label}: could not run {}: {e}",
                entry.command.join(" ")
            );
            Ok(false)
        }
    }
}

/// Restores a file and confirms its bytes. When that fails the saved copy is
/// kept, and the error names it.
fn restore_one(path: &Path, original: &[u8], saved: &Path) -> Result<(), String> {
    let restored = store::write_now(path, original).and_then(|()| fs::read(path));
    match restored {
        Ok(bytes) if bytes == original => Ok(()),
        Ok(_) => Err(format!(
            "{} did not restore byte for byte; its copy is kept at {}",
            path.display(),
            saved.display()
        )),
        Err(e) => Err(format!(
            "{} could not be restored ({e}); its copy is kept at {}",
            path.display(),
            saved.display()
        )),
    }
}

// ===========================================================================
// Restored runs
// ===========================================================================

/// Runs each distinct combination of command, environment and root once on the
/// restored tree. Each must pass with at least the largest `min_passed` of its
/// entries, within the largest timeout of its entries.
fn restored_runs(
    entries: &[Entry],
    groups: &[Vec<usize>],
    timeouts: &[Duration],
    log_dir: &Path,
) -> Result<bool, Stop> {
    let mut ok = true;
    for (k, group) in groups.iter().enumerate() {
        let e = &entries[group[0]];
        let need = group
            .iter()
            .map(|&i| entries[i].min_passed)
            .max()
            .unwrap_or(1);
        let limit = group.iter().map(|&i| timeouts[i]).max();
        let log = log_dir.join(format!("restored-{}.log", k + 1));
        let cmd = e.command.join(" ");
        match exec::run_command(
            &e.command,
            &e.env,
            &log,
            &e.root,
            limit,
            &interrupt::requested,
        ) {
            Ok((Ended::Interrupted, _)) => {
                return Err(Stop::Interrupted(
                    "interrupted during a restored run; no file is mutated".to_owned(),
                ));
            }
            Ok((Ended::TimedOut, _)) => {
                println!(
                    "TIMEOUT restored: no result after {}: {cmd} (see {})",
                    baseline::secs(limit.unwrap_or_default()),
                    log.display()
                );
                ok = false;
            }
            Ok((Ended::Exited, r)) => {
                let failing = r.run.failing();
                let passed = r.run.passed();
                if r.exit == Some(0) && failing.is_empty() && r.run.failed() == 0 && passed >= need
                {
                    println!("ok   restored: {passed} passed (need {need}): {cmd}");
                } else {
                    let exit = r
                        .exit
                        .map_or_else(|| "a signal".to_owned(), |c| c.to_string());
                    println!(
                        "FAIL restored: exit {exit}, {passed} passed (need {need}), failed {failing:?}: {cmd} (see {})",
                        log.display()
                    );
                    ok = false;
                }
            }
            Err(e) => {
                println!("FAIL restored: could not run {cmd}: {e}");
                ok = false;
            }
        }
    }
    Ok(ok)
}
