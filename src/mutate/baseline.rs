//  src/mutate/baseline.rs

//! The baseline runs each distinct command once on the unmutated tree before
//! any file is touched, and the default timeouts are derived from it.
//!
//! The baseline proves the tree was sound before mutation. Without it, a tree
//! that already fails makes every entry fail with the same unexpected tests,
//! which hides the real cause. It also measures how long a run takes, and that
//! measurement sets the default timeout.
//!
//! Every target file's modification time is set to now before the baseline
//! runs. Cargo then rebuilds those files' crates, so the measured time includes
//! the same incremental build a mutation run pays for, and one multiplier
//! covers both the build and the tests.

use std::path::Path;
use std::time::{Duration, Instant};

use super::exec::{self, Ended};
use super::store;
use super::table::Entry;

/// The default timeout is this many times the baseline's time.
pub(crate) const DEFAULT_MULTIPLIER: f64 = 5.0;
/// The default timeout is never less than this.
pub(crate) const DEFAULT_FLOOR: Duration = Duration::from_secs(60);
/// The baseline's own limit when no entry sets an explicit timeout.
const SAFETY_LIMIT: Duration = Duration::from_secs(30 * 60);

// ===========================================================================
// Grouping
// ===========================================================================

/// Whether two entries run the same command in the same way.
pub(crate) fn same_run(a: &Entry, b: &Entry) -> bool {
    a.command == b.command && a.env == b.env && a.root == b.root
}

/// The entries' indexes, grouped by run, in the order the runs first appear.
pub(crate) fn groups(entries: &[Entry]) -> Vec<Vec<usize>> {
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for (i, e) in entries.iter().enumerate() {
        match groups.iter_mut().find(|g| same_run(&entries[g[0]], e)) {
            Some(g) => g.push(i),
            None => groups.push(vec![i]),
        }
    }
    groups
}

// ===========================================================================
// Timeouts
// ===========================================================================

/// An entry's timeout when one is set. That is the entry's own or its table's,
/// both already merged into `entry.timeout`, and otherwise `--timeout`.
pub(crate) fn explicit(entry: &Entry, cli: Option<Duration>) -> Option<Duration> {
    entry.timeout.or(cli)
}

/// The timeout derived from a baseline that took `wall`.
pub(crate) fn derived(entry: &Entry, wall: Duration) -> Duration {
    let multiplier = entry.timeout_multiplier.unwrap_or(DEFAULT_MULTIPLIER);
    let floor = entry.timeout_floor.unwrap_or(DEFAULT_FLOOR);
    floor.max(wall.mul_f64(multiplier))
}

/// The ids of entries with no explicit timeout. `--skip-baseline` cannot run
/// them, because without a baseline there is nothing to derive a timeout from.
pub(crate) fn missing_timeouts(entries: &[Entry], cli: Option<Duration>) -> Vec<String> {
    entries
        .iter()
        .filter(|e| explicit(e, cli).is_none())
        .map(|e| e.id.clone())
        .collect()
}

/// Formats a duration as `20 s` or `1.5 s`.
pub(crate) fn secs(d: Duration) -> String {
    if d.subsec_millis() == 0 {
        format!("{} s", d.as_secs())
    } else {
        format!("{:.1} s", d.as_secs_f64())
    }
}

// ===========================================================================
// Running
// ===========================================================================

pub(crate) struct Baseline {
    pub wall: Duration,
    pub passed: u64,
}

pub(crate) enum BaselineError {
    /// The unmutated tree does not pass, or a file could not be touched.
    Fails(String),
    Interrupted,
}

/// Runs the baseline for one group of entries.
pub(crate) fn run(
    entries: &[Entry],
    group: &[usize],
    log: &Path,
    cli: Option<Duration>,
) -> Result<Baseline, BaselineError> {
    let first = &entries[group[0]];
    for &i in group {
        let path = entries[i].root.join(&entries[i].file);
        store::touch(&path)
            .map_err(|e| BaselineError::Fails(format!("cannot touch {}: {e}", path.display())))?;
    }
    let explicit: Option<Vec<Duration>> =
        group.iter().map(|&i| explicit(&entries[i], cli)).collect();
    let limit = explicit
        .and_then(|t| t.into_iter().max())
        .unwrap_or(SAFETY_LIMIT);
    let need = group
        .iter()
        .map(|&i| entries[i].min_passed)
        .max()
        .unwrap_or(1);
    let start = Instant::now();
    let (ended, r) = exec::run_command(
        &first.command,
        &first.env,
        log,
        &first.root,
        Some(limit),
        &crate::interrupt::requested,
    )
    .map_err(|e| BaselineError::Fails(format!("could not run {}: {e}", first.command.join(" "))))?;
    let wall = start.elapsed();
    let shown = log.display();
    match ended {
        Ended::Interrupted => Err(BaselineError::Interrupted),
        Ended::TimedOut => Err(BaselineError::Fails(format!(
            "the baseline did not finish within {} (see {shown})",
            secs(limit)
        ))),
        Ended::Exited if r.build_failed => Err(BaselineError::Fails(format!(
            "the unmutated tree did not build or printed no test results (see {shown})"
        ))),
        Ended::Exited if r.exit != Some(0) || r.run.failed() > 0 => {
            let exit = r
                .exit
                .map_or_else(|| "a signal".to_owned(), |c| c.to_string());
            Err(BaselineError::Fails(format!(
                "the unmutated tree already fails (exit {exit}, failing {:?}) (see {shown})",
                r.run.failing()
            )))
        }
        Ended::Exited if r.run.passed() < need => Err(BaselineError::Fails(format!(
            "only {} tests passed on the unmutated tree; the table needs at least {need} (see {shown})",
            r.run.passed()
        ))),
        Ended::Exited => Ok(Baseline {
            wall,
            passed: r.run.passed(),
        }),
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::{derived, explicit, groups, missing_timeouts, secs};
    use crate::mutate::table::Entry;
    use std::time::Duration;

    fn entry(id: &str, timeout: Option<u64>) -> Entry {
        Entry {
            id: id.to_owned(),
            timeout: timeout.map(Duration::from_secs),
            ..Entry::for_tests()
        }
    }

    #[test]
    fn timeout_precedence_entry_table_cli_derived() {
        // The entry's or table's timeout (merged at load) beats --timeout,
        // which beats the derived default.
        let cli = Some(Duration::from_secs(7));
        assert_eq!(
            explicit(&entry("a", Some(3)), cli),
            Some(Duration::from_secs(3))
        );
        assert_eq!(explicit(&entry("a", None), cli), cli);
        assert_eq!(explicit(&entry("a", None), None), None);
    }

    #[test]
    fn derived_uses_floor() {
        assert_eq!(
            derived(&entry("a", None), Duration::from_secs(2)),
            Duration::from_secs(60)
        );
        let e = Entry {
            timeout_floor: Some(Duration::from_secs(5)),
            ..entry("a", None)
        };
        assert_eq!(
            derived(&e, Duration::from_millis(200)),
            Duration::from_secs(5)
        );
    }

    #[test]
    fn derived_uses_multiplier() {
        assert_eq!(
            derived(&entry("a", None), Duration::from_secs(20)),
            Duration::from_secs(100)
        );
        let e = Entry {
            timeout_multiplier: Some(3.0),
            timeout_floor: Some(Duration::from_secs(1)),
            ..entry("a", None)
        };
        assert_eq!(derived(&e, Duration::from_secs(4)), Duration::from_secs(12));
    }

    #[test]
    fn skip_baseline_requires_timeouts() {
        let entries = [entry("a", Some(3)), entry("b", None)];
        assert_eq!(missing_timeouts(&entries, None), ["b"]);
        assert!(missing_timeouts(&entries, Some(Duration::from_secs(9))).is_empty());
    }

    #[test]
    fn entries_grouped_by_run() {
        let mut other = entry("c", None);
        other.command = vec!["cargo".to_owned(), "test".to_owned(), "--lib".to_owned()];
        assert_eq!(
            groups(&[entry("a", None), other, entry("b", None)]),
            [vec![0, 2], vec![1]]
        );
    }

    #[test]
    fn seconds_shown() {
        assert_eq!(secs(Duration::from_secs(20)), "20 s");
        assert_eq!(secs(Duration::from_millis(1500)), "1.5 s");
    }
}
