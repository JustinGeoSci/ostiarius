//  src/mutate/edit.rs

//! A mutation's edit is applied to a file's text under rules that make the edit
//! mean exactly one thing.
//!
//! The finds apply in order, each to the text the earlier ones left, and each
//! must occur exactly once in that text. Counting every find in the original
//! text instead would miss a replacement that creates or removes a later find's
//! occurrence, which would then apply twice or not at all.
//!
//! A find that spans lines is written with `\n` in the table. In a file whose
//! line endings are all CRLF, as in a Windows checkout, it matches as `\r\n`
//! and its replacement is written with `\r\n`, so the edit keeps the file's own
//! endings. In a file that mixes the two, which ending a find should match is
//! ambiguous, and the edit is refused.
//!
//! An edit that changes nothing is refused as well. Running the tests on an
//! unchanged file could only report that nothing caught the mutation, which
//! would name the wrong cause.

// ===========================================================================
// Errors
// ===========================================================================

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum EditError {
    /// Find number `index`, counted from 1, occurs `count` times rather than
    /// once.
    Count {
        index: usize,
        of: usize,
        find: String,
        count: usize,
        /// The count was taken after the earlier finds had been applied.
        after_earlier: bool,
        /// The find contains CRLF but the file's line endings are LF.
        crlf_in_lf_file: bool,
    },
    /// A find spanning lines in a file with both CRLF and LF endings.
    MixedEndings {
        index: usize,
        of: usize,
        find: String,
    },
    /// The edited text equals the original.
    NoChange,
}

impl EditError {
    /// The message, naming `file` as the table wrote it.
    pub fn describe(&self, file: &str) -> String {
        match self {
            Self::Count {
                index,
                of,
                find,
                count: 0,
                after_earlier: false,
                crlf_in_lf_file: false,
            } => format!(
                "the code this entry targets has changed: {} no longer occurs in {file}; re-examine the entry's claim against the current code, then update `find`",
                which(*index, *of, find)
            ),
            Self::Count {
                index,
                of,
                find,
                count,
                after_earlier,
                crlf_in_lf_file,
            } => {
                let mut why = format!(
                    "{} occurs {count} times in {file}",
                    which(*index, *of, find)
                );
                if *after_earlier {
                    why.push_str(" after the earlier finds were applied");
                }
                if *crlf_in_lf_file {
                    why.push_str(" (it contains CRLF; the file's line endings are LF)");
                }
                if *count > 1 {
                    why.push_str("; a find must occur exactly once, so add surrounding text to make it unique");
                }
                why
            }
            Self::MixedEndings { index, of, find } => format!(
                "{} spans lines, and {file} mixes CRLF and LF line endings, so which ending it should match is ambiguous",
                which(*index, *of, find)
            ),
            Self::NoChange => "the edit changes nothing (each replace equals its find)".to_owned(),
        }
    }
}

/// Shows a find as `find 2 ("...")` when an entry has several finds, and as
/// just `"..."` otherwise.
fn which(index: usize, of: usize, find: &str) -> String {
    let shown: String = find.chars().take(60).collect();
    let shown = if shown.len() < find.len() {
        format!("{shown}...")
    } else {
        shown
    };
    if of > 1 {
        format!("find {index} ({shown:?})")
    } else {
        format!("{shown:?}")
    }
}

// ===========================================================================
// Applying
// ===========================================================================

/// Applies `finds` and `replaces` to `text` in order.
pub(crate) fn apply_edit(
    text: &str,
    finds: &[String],
    replaces: &[String],
) -> Result<String, EditError> {
    let crlf = text.matches("\r\n").count();
    let mixed = crlf > 0 && crlf < text.matches('\n').count();
    let of = finds.len();
    let mut edited = text.to_owned();
    for (i, (find, replace)) in finds.iter().zip(replaces).enumerate() {
        let index = i + 1;
        let (find, replace) = if find.contains('\n') && !find.contains("\r\n") && crlf > 0 {
            if mixed {
                return Err(EditError::MixedEndings {
                    index,
                    of,
                    find: find.clone(),
                });
            }
            (
                find.replace('\n', "\r\n"),
                replace.replace("\r\n", "\n").replace('\n', "\r\n"),
            )
        } else {
            (find.clone(), replace.clone())
        };
        let count = edited.matches(find.as_str()).count();
        if count != 1 {
            return Err(EditError::Count {
                index,
                of,
                find,
                count,
                after_earlier: index > 1,
                crlf_in_lf_file: crlf == 0 && finds[i].contains("\r\n"),
            });
        }
        edited = edited.replacen(find.as_str(), &replace, 1);
    }
    if edited == text {
        return Err(EditError::NoChange);
    }
    Ok(edited)
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::{EditError, apply_edit};

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn single_find() {
        assert_eq!(
            apply_edit("a <= b", &v(&["<="]), &v(&["<"])),
            Ok("a < b".to_owned())
        );
    }

    #[test]
    fn finds_in_order() {
        let text = "let x = 1;\nlet y = 2;\n";
        let out = apply_edit(text, &v(&["x = 1", "y = 2"]), &v(&["x = 3", "y = 4"]));
        assert_eq!(out, Ok("let x = 3;\nlet y = 4;\n".to_owned()));
    }

    #[test]
    fn later_find_count_changed_by_earlier() {
        // "b.1" occurs once in the original, twice after the first replacement.
        let text = "a.0 <= b.1 && b.0 <= a.1";
        let err = apply_edit(
            text,
            &v(&["a.0 <= b.1", "b.1"]),
            &v(&["a.0 <= b.1 && a.0 <= b.1", "b.0"]),
        );
        let Err(EditError::Count {
            index: 2,
            count: 2,
            after_earlier: true,
            ..
        }) = err
        else {
            panic!("expected a count error on find 2, got {err:?}");
        };
    }

    #[test]
    fn crlf_file_multiline_find_matched() {
        let text = "fn f() {\r\n    let k = 3;\r\n    k\r\n}\r\n";
        let out = apply_edit(text, &v(&["let k = 3;\n    k"]), &v(&["let k = 2;\n    k"]));
        assert_eq!(
            out,
            Ok("fn f() {\r\n    let k = 2;\r\n    k\r\n}\r\n".to_owned())
        );
    }

    #[test]
    fn crlf_replacement_written_crlf() {
        let text = "a\r\nb\r\n";
        let out = apply_edit(text, &v(&["a\nb"]), &v(&["a\nx\ny"]));
        assert_eq!(out, Ok("a\r\nx\r\ny\r\n".to_owned()));
    }

    #[test]
    fn mixed_endings_refused() {
        let text = "a\r\nb\nc\n";
        assert!(matches!(
            apply_edit(text, &v(&["b\nc"]), &v(&["b\nd"])),
            Err(EditError::MixedEndings { .. })
        ));
        // A single-line find in the same file is fine.
        assert_eq!(
            apply_edit(text, &v(&["c"]), &v(&["d"])),
            Ok("a\r\nb\nd\n".to_owned())
        );
    }

    #[test]
    fn crlf_find_in_lf_file_hinted() {
        let err = apply_edit("a\nb\n", &v(&["a\r\nb"]), &v(&["x"])).expect_err("refused");
        assert!(
            err.describe("f.rs")
                .contains("(it contains CRLF; the file's line endings are LF)")
        );
    }

    #[test]
    fn no_change_refused() {
        assert_eq!(
            apply_edit("let k = 3;", &v(&["k = 3"]), &v(&["k = 3"])),
            Err(EditError::NoChange)
        );
    }

    #[test]
    fn zero_occurrences_is_stale_message() {
        let err = apply_edit("a < b", &v(&["a <= b"]), &v(&["a < b"])).expect_err("refused");
        let msg = err.describe("src/x.rs");
        assert!(
            msg.starts_with(
                "the code this entry targets has changed: \"a <= b\" no longer occurs in src/x.rs"
            ),
            "{msg}"
        );
    }

    #[test]
    fn two_occurrences_named() {
        let err = apply_edit("x x", &v(&["x"]), &v(&["y"])).expect_err("refused");
        let msg = err.describe("f.rs");
        assert!(
            msg.starts_with("\"x\" occurs 2 times in f.rs; a find must occur exactly once"),
            "{msg}"
        );
    }

    #[test]
    fn long_find_is_shortened_in_messages() {
        let long = "x".repeat(80);
        let err = apply_edit("y", std::slice::from_ref(&long), &v(&["z"])).expect_err("refused");
        assert!(
            err.describe("f.rs")
                .contains(&format!("{}...", "x".repeat(60)))
        );
    }
}
