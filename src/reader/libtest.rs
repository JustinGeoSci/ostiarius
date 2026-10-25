//  src/reader/libtest.rs

//! This module reads libtest's human-readable output as `cargo test` prints it.
//! The lines that matter look like this.
//!
//! ```text
//!      Running unittests src/lib.rs (target/debug/deps/demo-1a2b3c4d5e6f7a8b)
//! test tests::adds ... ok
//! test tests::subtracts ... FAILED
//! ---- tests::subtracts stdout ----
//! <whatever the test printed>
//! failures:
//!     tests::subtracts
//! test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
//!    Doc-tests demo
//! ```
//!
//! A failing test's captured output is printed between its
//! `---- <name> stdout ----` header and the next `failures:` line, or
//! `successes:` under `--show-output`. A test can print anything in that block, including a line
//! shaped like a result, so the block is skipped entirely. Cargo prints the
//! `Running` and `Doc-tests` headers on stderr while libtest prints its results
//! on stdout, and only a log that interleaves the two faithfully lets each
//! result be attributed to the binary that produced it.

use super::{Binary, Outcome, Reader, Run};

// ===========================================================================
// The reader
// ===========================================================================

pub(crate) struct Libtest;

impl Reader for Libtest {
    fn parse(&self, text: &str) -> Run {
        let mut run = Run::default();
        let mut captured = false;
        let mut headers = 0_usize;
        let mut pending: Option<String> = None;
        let mut tests: Vec<(String, Outcome)> = Vec::new();
        for raw in text.lines() {
            let line = raw.trim_end();
            if is_capture_start(line) {
                captured = true;
                continue;
            }
            if captured {
                if line == "failures:" || line == "successes:" {
                    captured = false;
                }
                continue;
            }
            let trimmed = line.trim_start();
            if let Some(rest) = trimmed.strip_prefix("Running ") {
                headers += 1;
                pending = Some(label_from_running(rest));
            } else if let Some(krate) = trimmed.strip_prefix("Doc-tests ") {
                headers += 1;
                pending = Some(format!("doctests ({})", krate.trim()));
            } else if let Some((passed, failed, ignored)) = result_counts(line) {
                let label = pending
                    .take()
                    .unwrap_or_else(|| format!("(unlabeled {})", run.binaries.len() + 1));
                run.binaries.push(Binary {
                    label,
                    passed,
                    failed,
                    ignored,
                    tests: std::mem::take(&mut tests),
                });
            } else if let Some(test) = test_line(line) {
                tests.push(test);
            }
        }
        if headers > 0 && run.binaries.len() > headers {
            run.suspect = Some(format!(
                "it has {} result lines but only {headers} test binaries; a result line was printed by something other than the test harness",
                run.binaries.len()
            ));
        }
        run
    }

    fn parse_list(&self, text: &str) -> Vec<String> {
        let mut names: Vec<String> = text
            .lines()
            .filter_map(|line| line.trim_end().strip_suffix(": test"))
            .map(normalize_name)
            .collect();
        names.sort();
        names.dedup();
        names
    }
}

// ===========================================================================
// Names and labels
// ===========================================================================

/// A test's name as users write it. A doctest prints as
/// `src/lib.rs - Item (line 32)`, but its line number moves with every edit
/// above it and its path has backslashes on Windows, so both are normalized
/// away to give `src/lib.rs - Item`.
pub(crate) fn normalize_name(name: &str) -> String {
    let name = name.trim();
    let name = strip_line_suffix(name);
    match name.split_once(" - ") {
        Some((path, item)) => format!("{} - {item}", path.replace('\\', "/")),
        None => name.to_owned(),
    }
}

fn strip_line_suffix(name: &str) -> &str {
    let Some(open) = name.rfind(" (line ") else {
        return name;
    };
    match name[open + " (line ".len()..].strip_suffix(')') {
        Some(digits) if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) => {
            &name[..open]
        }
        _ => name,
    }
}

/// Turns `unittests src/lib.rs (target/debug/deps/demo-1a2b3c4d5e6f7a8b)` into
/// `unittests src/lib.rs (demo)`. The executable's stem names the crate or test
/// target, which Cargo's target text alone does not, since two packages in one
/// workspace can both have `unittests src/lib.rs`. The hash changes with every
/// build and is dropped, and path separators become `/` so that a label reads
/// the same on every platform.
fn label_from_running(rest: &str) -> String {
    let rest = rest.trim();
    let (target, path) = match rest.strip_suffix(')').and_then(|r| r.rsplit_once(" (")) {
        Some((target, path)) => (target, path),
        // Older Cargo printed only the executable's path.
        None => ("", rest),
    };
    let stem = stem_of(path);
    if target.is_empty() {
        format!("({stem})")
    } else {
        format!("{} ({stem})", target.replace('\\', "/"))
    }
}

fn stem_of(path: &str) -> &str {
    let file = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let file = file.strip_suffix(".exe").unwrap_or(file);
    match file.rsplit_once('-') {
        Some((stem, hash)) if hash.len() >= 8 && hash.bytes().all(|b| b.is_ascii_hexdigit()) => {
            stem
        }
        _ => file,
    }
}

// ===========================================================================
// Lines
// ===========================================================================

fn is_capture_start(line: &str) -> bool {
    line.starts_with("---- ") && (line.ends_with(" stdout ----") || line.ends_with(" stderr ----"))
}

/// Reads `test result: ok. 4 passed; 0 failed; 1 ignored; ...` as `(4, 0, 1)`.
fn result_counts(line: &str) -> Option<(u64, u64, u64)> {
    let rest = line.strip_prefix("test result: ")?;
    let rest = rest
        .strip_prefix("ok. ")
        .or_else(|| rest.strip_prefix("FAILED. "))?;
    let mut parts = rest.split("; ");
    let passed = count_of(parts.next()?, "passed")?;
    let failed = count_of(parts.next()?, "failed")?;
    let ignored = count_of(parts.next()?, "ignored")?;
    Some((passed, failed, ignored))
}

fn count_of(part: &str, word: &str) -> Option<u64> {
    part.strip_suffix(word)?.trim_end().parse().ok()
}

/// Reads `test <name> ... ok`, `... FAILED`, `... ignored` or `... ignored,
/// <reason>`.
fn test_line(line: &str) -> Option<(String, Outcome)> {
    let rest = line.strip_prefix("test ")?;
    let (name, outcome) = rest.rsplit_once(" ... ")?;
    let outcome = match outcome {
        "ok" => Outcome::Ok,
        "FAILED" => Outcome::Failed,
        "ignored" => Outcome::Ignored,
        other if other.starts_with("ignored, ") => Outcome::Ignored,
        _ => return None,
    };
    Some((normalize_name(name), outcome))
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::super::{Outcome, Reader};
    use super::{Libtest, label_from_running, normalize_name};

    const RESULT_OK: &str = "test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s";

    fn parse(text: &str) -> super::super::Run {
        Libtest.parse(text)
    }

    #[test]
    fn single_binary_counts() {
        let text = format!(
            "     Running unittests src/lib.rs (target/debug/deps/demo-0123456789abcdef)\n\nrunning 2 tests\ntest a ... ok\ntest b ... ok\n\n{RESULT_OK}\n"
        );
        let run = parse(&text);
        assert_eq!(run.binaries.len(), 1);
        assert_eq!(run.passed(), 2);
        assert_eq!(run.binaries[0].label, "unittests src/lib.rs (demo)");
        assert_eq!(
            run.binaries[0].tests,
            [("a".to_owned(), Outcome::Ok), ("b".to_owned(), Outcome::Ok)]
        );
        assert!(run.suspect.is_none());
    }

    #[test]
    fn multi_binary_labels() {
        let text = format!(
            "     Running unittests src/lib.rs (target/debug/deps/demo-0123456789abcdef)\n{RESULT_OK}\n     Running unittests src/lib.rs (target/debug/deps/other-fedcba9876543210)\n{RESULT_OK}\n     Running tests/basic.rs (target/debug/deps/basic-0011223344556677)\n{RESULT_OK}\n"
        );
        let labels: Vec<String> = parse(&text).binaries.into_iter().map(|b| b.label).collect();
        assert_eq!(
            labels,
            [
                "unittests src/lib.rs (demo)",
                "unittests src/lib.rs (other)",
                "tests/basic.rs (basic)"
            ]
        );
    }

    #[test]
    fn doctest_label_and_name_normalized() {
        let text = format!(
            "   Doc-tests demo\n\nrunning 1 test\ntest src\\lib.rs - add (line 12) ... ok\n\n{RESULT_OK}\n"
        );
        let run = parse(&text);
        assert_eq!(run.binaries[0].label, "doctests (demo)");
        assert_eq!(run.binaries[0].tests[0].0, "src/lib.rs - add");
    }

    fn spoofed(newline: &str) -> String {
        [
            "running 2 tests",
            "test a ... ok",
            "test b ... FAILED",
            "",
            "failures:",
            "",
            "---- b stdout ----",
            "test c ... FAILED",
            "test result: ok. 999 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s",
            "",
            "---- b stderr ----",
            "test d ... FAILED",
            "",
            "failures:",
            "    b",
            "",
            "test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s",
            "",
        ]
        .join(newline)
    }

    #[test]
    fn captured_output_is_skipped_lf() {
        let run = parse(&spoofed("\n"));
        assert_eq!((run.passed(), run.failed(), run.binaries.len()), (1, 1, 1));
        assert_eq!(run.failing(), ["b"]);
    }

    #[test]
    fn captured_output_is_skipped_crlf() {
        let run = parse(&spoofed("\r\n"));
        assert_eq!((run.passed(), run.failed(), run.binaries.len()), (1, 1, 1));
        assert_eq!(run.failing(), ["b"]);
    }

    #[test]
    fn show_output_successes_block_skipped() {
        let text = format!(
            "running 2 tests\ntest a ... ok\ntest b ... ok\n\nsuccesses:\n\n---- a stdout ----\ntest result: ok. 50 passed; 0 failed; 0 ignored\n\nsuccesses:\n    a\n    b\n\n{RESULT_OK}\n"
        );
        let run = parse(&text);
        assert_eq!((run.passed(), run.binaries.len()), (2, 1));
    }

    #[test]
    fn ignored_with_reason_is_ignored() {
        let text = "test slow ... ignored, takes an hour\ntest result: ok. 0 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.00s\n";
        let run = parse(text);
        assert_eq!(
            run.binaries[0].tests,
            [("slow".to_owned(), Outcome::Ignored)]
        );
        assert_eq!(run.ignored(), 1);
    }

    #[test]
    fn more_results_than_headers_is_suspect() {
        let text = format!(
            "     Running unittests src/lib.rs (target/debug/deps/demo-0123456789abcdef)\n{RESULT_OK}\n{RESULT_OK}\n"
        );
        assert!(parse(&text).suspect.is_some());
    }

    #[test]
    fn no_headers_gives_unlabeled_binaries() {
        let run = parse(&format!("{RESULT_OK}\n{RESULT_OK}\n"));
        assert!(run.suspect.is_none());
        let labels: Vec<String> = run.binaries.into_iter().map(|b| b.label).collect();
        assert_eq!(labels, ["(unlabeled 1)", "(unlabeled 2)"]);
    }

    #[test]
    fn label_strips_hash_and_exe() {
        assert_eq!(
            label_from_running(r"tests\basic.rs (target\release\deps\basic-0123456789abcdef.exe)"),
            "tests/basic.rs (basic)"
        );
        assert_eq!(
            label_from_running(
                "unittests src/main.rs (target/debug/deps/my_tool-0123456789abcdef)"
            ),
            "unittests src/main.rs (my_tool)"
        );
        assert_eq!(
            label_from_running("target/debug/deps/demo-0123456789abcdef"),
            "(demo)"
        );
        // A dash followed by something that is not a hash stays.
        assert_eq!(
            label_from_running("tests/a.rs (target/debug/deps/my-tests)"),
            "tests/a.rs (my-tests)"
        );
    }

    #[test]
    fn result_line_needs_its_counts() {
        let run = parse("test result: ok. many passed\ntest result: ok. 1 passed; 0 failed\n");
        assert!(run.binaries.is_empty());
    }

    #[test]
    fn list_terse_parsed() {
        let text = "     Running unittests src/lib.rs (target/debug/deps/demo-0123456789abcdef)\ntests::b: test\ntests::a: test\nbench_x: benchmark\n\n2 tests, 1 benchmark\n";
        assert_eq!(Libtest.parse_list(text), ["tests::a", "tests::b"]);
    }

    #[test]
    fn list_doctest_names_normalized() {
        let text = "   Doc-tests demo\nsrc\\lib.rs - add (line 3): test\n";
        assert_eq!(Libtest.parse_list(text), ["src/lib.rs - add"]);
    }

    #[test]
    fn normalize_keeps_ordinary_names() {
        assert_eq!(normalize_name("tests::a (line x)"), "tests::a (line x)");
        assert_eq!(normalize_name("  tests::a  "), "tests::a");
    }
}
