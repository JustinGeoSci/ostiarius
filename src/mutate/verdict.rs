//  src/mutate/verdict.rs

//! An entry passes only if its mutation failed exactly the tests it names, and
//! this module judges that.

use std::path::Path;

use super::exec::CommandResult;
use super::table::{Entry, Expect};

// ===========================================================================
// Verdicts
// ===========================================================================

/// The start of the entry's line, which reads `ok <id> <what>: ...` or
/// `FAIL <id> <what>: ...`.
pub(crate) fn label(entry: &Entry) -> String {
    format!("{:<24} {}", entry.id, entry.what)
}

fn short(names: &[String]) -> String {
    names
        .iter()
        .map(|n| n.rsplit("::").next().unwrap_or(n))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Returns whether the entry passed, and its line.
pub(crate) fn judge(entry: &Entry, result: &CommandResult, log: &Path) -> (bool, String) {
    let label = label(entry);
    let log = log.display();
    match entry.expect {
        Expect::CompileError => {
            if result.build_failed && result.compiler_error {
                (true, format!("ok   {label}: rejected by the compiler"))
            } else if result.build_failed {
                (
                    false,
                    format!(
                        "FAIL {label}: the command failed without test results, but not with a compiler error (see {log})"
                    ),
                )
            } else {
                (
                    false,
                    format!("FAIL {label}: expected a build error; it built (see {log})"),
                )
            }
        }
        Expect::TestsFail => judge_tests(entry, result, &label, &log.to_string()),
    }
}

fn judge_tests(entry: &Entry, result: &CommandResult, label: &str, log: &str) -> (bool, String) {
    if result.build_failed {
        return (
            false,
            format!("FAIL {label}: the mutated tree did not build (see {log})"),
        );
    }
    if result.exit == Some(0) {
        return (
            false,
            format!(
                "FAIL {label}: the command passed ({} passed); nothing caught it (see {log})",
                result.run.passed()
            ),
        );
    }
    let failing = result.run.failing();
    let missing: Vec<String> = entry
        .must_fail
        .iter()
        .filter(|t| !failing.contains(t))
        .cloned()
        .collect();
    let unexpected: Vec<String> = failing
        .iter()
        .filter(|t| !entry.must_fail.contains(t) && !entry.may_fail.contains(t))
        .cloned()
        .collect();
    if !missing.is_empty() || !unexpected.is_empty() {
        return (
            false,
            format!("FAIL {label}: missing {missing:?}, unexpected {unexpected:?} (see {log})"),
        );
    }
    let named: Vec<String> = failing
        .iter()
        .filter(|t| entry.must_fail.contains(t))
        .cloned()
        .collect();
    let also: Vec<String> = failing
        .iter()
        .filter(|t| entry.may_fail.contains(t))
        .cloned()
        .collect();
    let mut line = format!("ok   {label}: fails {}", short(&named));
    if !also.is_empty() {
        line = format!("{line}; also {} (may fail)", short(&also));
    }
    (true, line)
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::judge;
    use crate::mutate::exec::CommandResult;
    use crate::mutate::table::{Entry, Expect};
    use crate::reader::{Binary, Outcome, Run};
    use std::path::Path;

    fn entry(must: &[&str], may: &[&str], expect: Expect) -> Entry {
        Entry {
            id: "m1".to_owned(),
            what: "a mutation".to_owned(),
            must_fail: must.iter().map(|s| (*s).to_owned()).collect(),
            may_fail: may.iter().map(|s| (*s).to_owned()).collect(),
            expect,
            ..Entry::for_tests()
        }
    }

    fn result(exit: Option<i32>, failing: &[&str], compiler_error: bool) -> CommandResult {
        let built = !(exit != Some(0) && failing.is_empty() && compiler_error);
        let run = if built {
            let tests = failing
                .iter()
                .map(|n| ((*n).to_owned(), Outcome::Failed))
                .collect();
            Run {
                binaries: vec![Binary {
                    label: "x".to_owned(),
                    passed: 3,
                    failed: failing.len() as u64,
                    ignored: 0,
                    tests,
                }],
                suspect: None,
            }
        } else {
            Run::default()
        };
        CommandResult {
            exit,
            build_failed: !built,
            compiler_error,
            run,
        }
    }

    fn line(e: &Entry, r: &CommandResult) -> (bool, String) {
        judge(e, r, Path::new("m1.log"))
    }

    #[test]
    fn did_not_build_fails() {
        let (ok, line) = line(
            &entry(&["a"], &[], Expect::TestsFail),
            &result(Some(101), &[], true),
        );
        assert!(!ok);
        assert!(line.contains("the mutated tree did not build"), "{line}");
    }

    #[test]
    fn built_when_compile_error_expected_fails() {
        let (ok, line) = line(
            &entry(&[], &[], Expect::CompileError),
            &result(Some(101), &["a"], false),
        );
        assert!(!ok);
        assert!(line.contains("expected a build error; it built"), "{line}");
    }

    #[test]
    fn compile_error_expected_ok() {
        let (ok, line) = line(
            &entry(&[], &[], Expect::CompileError),
            &result(Some(101), &[], true),
        );
        assert!(ok, "{line}");
        assert!(line.ends_with("rejected by the compiler"));
    }

    #[test]
    fn command_passed_fails() {
        let (ok, line) = line(
            &entry(&["a"], &[], Expect::TestsFail),
            &result(Some(0), &[], false),
        );
        assert!(!ok);
        assert!(
            line.contains("the command passed (3 passed); nothing caught it"),
            "{line}"
        );
    }

    #[test]
    fn missing_named_failure_fails() {
        let (ok, line) = line(
            &entry(&["a", "b"], &[], Expect::TestsFail),
            &result(Some(101), &["a"], false),
        );
        assert!(!ok);
        assert!(line.contains(r#"missing ["b"], unexpected []"#), "{line}");
    }

    #[test]
    fn unexpected_failure_fails() {
        let (ok, line) = line(
            &entry(&["a"], &[], Expect::TestsFail),
            &result(Some(101), &["a", "c"], false),
        );
        assert!(!ok);
        assert!(line.contains(r#"missing [], unexpected ["c"]"#), "{line}");
    }

    #[test]
    fn exact_failures_ok() {
        let (ok, line) = line(
            &entry(&["mod::a"], &[], Expect::TestsFail),
            &result(Some(101), &["mod::a"], false),
        );
        assert!(ok, "{line}");
        assert!(line.ends_with(": fails a"), "{line}");
    }

    #[test]
    fn may_fail_allowed_and_reported() {
        let e = entry(&["a"], &["r"], Expect::TestsFail);
        assert!(line(&e, &result(Some(101), &["a"], false)).0);
        let (ok, line) = line(&e, &result(Some(101), &["a", "r"], false));
        assert!(ok, "{line}");
        assert!(line.ends_with("; also r (may fail)"), "{line}");
    }

    #[test]
    fn signal_without_results_is_did_not_build() {
        let (ok, line) = line(
            &entry(&["a"], &[], Expect::TestsFail),
            &result(None, &[], true),
        );
        assert!(!ok);
        assert!(line.contains("did not build"), "{line}");
    }
}
