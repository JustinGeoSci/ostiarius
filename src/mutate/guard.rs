//  src/mutate/guard.rs

//! A run refuses a tree that may already hold a mutation, because a run that
//! was killed outright could not restore the file it was mutating. Two checks,
//! made before the baseline and before any file is touched, keep such a
//! leftover from being built on.
//!
//! The first check looks for saved copies left behind. Every mutation is
//! preceded by a saved copy that is removed only once the file is restored, so
//! a copy that is still there means a run stopped before restoring.
//!
//! The second check looks for uncommitted changes in a target file when the
//! root is in a git work tree. A leftover mutation looks exactly like an
//! uncommitted change, so a target file must match its committed version unless
//! the user says otherwise with `--allow-dirty`. An untracked target file
//! counts as changed, since there is no committed version to compare it with.
//!
//! Without git the second check is skipped with a note, while the first check
//! and the baseline still apply.

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::store;
use super::table::Entry;

// ===========================================================================
// Saved copies
// ===========================================================================

/// The refusal message, when the store holds saved copies.
pub(crate) fn leftovers(keep: &Path) -> io::Result<Option<String>> {
    let found = store::leftovers(keep)?;
    if found.is_empty() {
        return Ok(None);
    }
    let mut msg = "an earlier run was stopped before it restored these files:".to_owned();
    for (copy, dst) in found {
        let line = match dst {
            Some(dst) => format!("\n  {} (saved copy: {})", dst.display(), copy.display()),
            None => format!(
                "\n  (no destination recorded) saved copy: {}",
                copy.display()
            ),
        };
        msg += &line;
    }
    msg.push_str("\nRun `ostiarius mutate --restore` with the same --log-dir, check the files, then run again.");
    Ok(Some(msg))
}

// ===========================================================================
// Uncommitted changes
// ===========================================================================

/// One `git status --porcelain` line per changed path.
fn parse_porcelain(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim_end)
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect()
}

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The changed target files under `root`, or `None` when `root` is not in a git
/// work tree or git cannot be run.
fn changed(root: &Path, files: &[&Path]) -> Option<Vec<String>> {
    if git(root, &["rev-parse", "--is-inside-work-tree"])?.trim() != "true" {
        return None;
    }
    let mut args: Vec<String> = ["status", "--porcelain", "--untracked-files=all", "--"]
        .map(String::from)
        .to_vec();
    args.extend(files.iter().map(|f| f.to_string_lossy().replace('\\', "/")));
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    git(root, &args).map(|text| parse_porcelain(&text))
}

/// Notes for the roots the check could not cover, and the refusal message when
/// any target file has uncommitted changes.
pub(crate) fn dirty(entries: &[Entry]) -> (Vec<String>, Option<String>) {
    let mut roots: Vec<&PathBuf> = Vec::new();
    for e in entries {
        if !roots.contains(&&e.root) {
            roots.push(&e.root);
        }
    }
    let mut notes = Vec::new();
    let mut lines = Vec::new();
    for root in roots {
        let mut files: Vec<&Path> = entries
            .iter()
            .filter(|e| &e.root == root)
            .map(|e| e.file.as_path())
            .collect();
        files.sort();
        files.dedup();
        match changed(root, &files) {
            Some(found) => lines.extend(found.into_iter().map(|l| format!("  {l}    (in {})", root.display()))),
            None => notes.push(format!(
                "{} is not in a git work tree (or git is not installed); the uncommitted-changes check was skipped",
                root.display()
            )),
        }
    }
    if lines.is_empty() {
        return (notes, None);
    }
    let msg = format!(
        "these target files have uncommitted changes (`??` is untracked), and a mutation left behind by a stopped run would look the same:\n{}\nCommit or stash them first, or run with --allow-dirty.",
        lines.join("\n")
    );
    (notes, Some(msg))
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::{dirty, leftovers, parse_porcelain};
    use crate::mutate::store;
    use crate::mutate::table::Entry;
    use std::path::Path;

    fn temp(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("ostiarius-guard-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).expect("temp dir");
        dir
    }

    #[test]
    fn leftovers_message_lists_destinations() {
        let dir = temp("leftovers");
        let keep = dir.join(".originals");
        assert_eq!(leftovers(&keep).expect("checked"), None);
        store::save(&keep, &dir, Path::new("src/lib.rs"), b"x").expect("saved");
        let msg = leftovers(&keep).expect("checked").expect("refused");
        assert!(
            msg.starts_with("an earlier run was stopped before it restored these files:"),
            "{msg}"
        );
        assert!(
            msg.contains(&dir.join("src/lib.rs").display().to_string()),
            "{msg}"
        );
        assert!(msg.contains("ostiarius mutate --restore"), "{msg}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn porcelain_line_parsed() {
        assert_eq!(
            parse_porcelain(" M src/lib.rs\n?? src/new.rs\n\n"),
            [" M src/lib.rs", "?? src/new.rs"]
        );
        assert!(parse_porcelain("").is_empty());
    }

    #[test]
    fn untracked_counts_as_dirty() {
        // A status line for an untracked file is a change like any other.
        assert_eq!(parse_porcelain("?? src/lib.rs\n").len(), 1);
    }

    #[test]
    fn not_a_work_tree_is_skipped_with_note() {
        let dir = temp("not-git");
        let entry = Entry {
            root: dir.clone(),
            ..Entry::for_tests()
        };
        let (notes, refusal) = dirty(&[entry]);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(refusal.is_none());
        assert_eq!(notes.len(), 1);
        assert!(
            notes[0].ends_with("the uncommitted-changes check was skipped"),
            "{notes:?}"
        );
    }
}
