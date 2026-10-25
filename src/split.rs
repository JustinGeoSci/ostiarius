//  src/split.rs

//! A command written as one string is split into its arguments by the POSIX
//! shell's quoting rules, without any expansion.
//!
//! Unquoted whitespace separates words, and single quotes keep everything
//! between them literally. Inside double quotes a backslash escapes only `"`,
//! `\`, `$` and `` ` ``, while outside quotes it escapes any character. The
//! same rules remove the backslashes from a Windows path written without
//! quotes, which is why a table should give its command as a list of arguments
//! wherever it can.

use std::fmt;

// ===========================================================================
// Splitting
// ===========================================================================

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SplitError {
    UnterminatedQuote(char),
    TrailingBackslash,
}

impl fmt::Display for SplitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnterminatedQuote(q) => write!(f, "the quote {q} is never closed"),
            Self::TrailingBackslash => write!(f, "the command ends with a lone backslash"),
        }
    }
}

/// Splits `command` into arguments by POSIX quoting rules.
pub(crate) fn split(command: &str) -> Result<Vec<String>, SplitError> {
    let mut args = Vec::new();
    let mut word = String::new();
    // A word exists once anything, even an empty pair of quotes, starts it.
    let mut in_word = false;
    let mut chars = command.chars();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {
                if in_word {
                    args.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            '\'' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(inner) => word.push(inner),
                        None => return Err(SplitError::UnterminatedQuote('\'')),
                    }
                }
            }
            '"' => {
                in_word = true;
                double_quoted(&mut chars, &mut word)?;
            }
            '\\' => {
                in_word = true;
                word.push(chars.next().ok_or(SplitError::TrailingBackslash)?);
            }
            other => {
                in_word = true;
                word.push(other);
            }
        }
    }
    if in_word {
        args.push(word);
    }
    Ok(args)
}

fn double_quoted(chars: &mut std::str::Chars<'_>, word: &mut String) -> Result<(), SplitError> {
    loop {
        match chars.next() {
            Some('"') => return Ok(()),
            Some('\\') => match chars.next() {
                Some(escaped @ ('"' | '\\' | '$' | '`')) => word.push(escaped),
                Some(other) => {
                    word.push('\\');
                    word.push(other);
                }
                None => return Err(SplitError::UnterminatedQuote('"')),
            },
            Some(inner) => word.push(inner),
            None => return Err(SplitError::UnterminatedQuote('"')),
        }
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::{SplitError, split};

    fn ok(command: &str) -> Vec<String> {
        split(command).expect("splits")
    }

    #[test]
    fn plain_words() {
        assert_eq!(
            ok("cargo test  -p demo\t--release"),
            ["cargo", "test", "-p", "demo", "--release"]
        );
    }

    #[test]
    fn double_quotes_group() {
        assert_eq!(ok(r#"cargo test "a b" c"#), ["cargo", "test", "a b", "c"]);
    }

    #[test]
    fn single_quotes_are_literal() {
        assert_eq!(ok(r"echo 'a \ $b'"), ["echo", r"a \ $b"]);
    }

    #[test]
    fn backslash_escapes_outside_quotes() {
        assert_eq!(ok(r"a\ b c\\d"), ["a b", r"c\d"]);
    }

    #[test]
    fn double_quote_keeps_other_backslashes() {
        assert_eq!(ok(r#""C:\dev \"x\"""#), [r#"C:\dev "x""#]);
    }

    #[test]
    fn empty_string_is_empty() {
        assert!(ok("   ").is_empty());
        assert_eq!(ok(r#"a "" b"#), ["a", "", "b"]);
    }

    #[test]
    fn unterminated_quote_is_error() {
        assert_eq!(split("a 'b"), Err(SplitError::UnterminatedQuote('\'')));
        assert_eq!(split(r#"a "b"#), Err(SplitError::UnterminatedQuote('"')));
        assert_eq!(split("a \\"), Err(SplitError::TrailingBackslash));
    }

    #[test]
    fn adjacent_quoted_parts_join() {
        assert_eq!(ok(r#"--x='a b'"c"d"#), ["--x=a bcd"]);
    }
}
