//  src/cli.rs

//! Both subcommands share this small argument parser.
//!
//! Options are long only (`--name`) and take their value either as the next
//! argument or after `=`. Every option is declared up front, so an unknown or
//! misspelled option is reported as an error rather than taken for a file name.
//! Everything after a bare `--` is a command and passes through untouched.

use std::collections::BTreeMap;
use std::fmt;

// ===========================================================================
// Declarations and results
// ===========================================================================

/// One option a subcommand accepts.
pub(crate) struct Opt {
    pub name: &'static str,
    pub takes_value: bool,
    pub repeatable: bool,
}

impl Opt {
    pub const fn flag(name: &'static str) -> Self {
        Self {
            name,
            takes_value: false,
            repeatable: false,
        }
    }

    pub const fn value(name: &'static str) -> Self {
        Self {
            name,
            takes_value: true,
            repeatable: false,
        }
    }

    pub const fn repeated(name: &'static str) -> Self {
        Self {
            name,
            takes_value: true,
            repeatable: true,
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct Parsed {
    values: BTreeMap<&'static str, Vec<String>>,
    flags: Vec<&'static str>,
    pub positional: Vec<String>,
    /// The arguments after `--`, when `--` was given.
    pub command: Option<Vec<String>>,
}

impl Parsed {
    pub fn flag(&self, name: &str) -> bool {
        self.flags.contains(&name)
    }

    pub fn value(&self, name: &str) -> Option<&str> {
        self.values
            .get(name)
            .and_then(|v| v.first())
            .map(String::as_str)
    }

    pub fn values(&self, name: &str) -> &[String] {
        self.values.get(name).map_or(&[], Vec::as_slice)
    }
}

/// A command-line mistake, which always exits with the usage code, 3.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct UsageError(pub String);

impl fmt::Display for UsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

// ===========================================================================
// Parsing
// ===========================================================================

pub(crate) fn parse(args: &[String], opts: &[Opt]) -> Result<Parsed, UsageError> {
    let mut parsed = Parsed::default();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        i += 1;
        if arg == "--" {
            parsed.command = Some(args[i..].to_vec());
            break;
        }
        let Some(body) = arg.strip_prefix("--") else {
            parsed.positional.push(arg.clone());
            continue;
        };
        let (name, inline) = match body.split_once('=') {
            Some((name, value)) => (name, Some(value.to_owned())),
            None => (body, None),
        };
        let Some(opt) = opts.iter().find(|o| o.name == name) else {
            return Err(UsageError(format!("unknown option --{name}")));
        };
        if opt.takes_value {
            let value = if let Some(v) = inline {
                v
            } else {
                let v = args
                    .get(i)
                    .ok_or_else(|| UsageError(format!("--{name} needs a value")))?;
                i += 1;
                v.clone()
            };
            let slot = parsed.values.entry(opt.name).or_default();
            if !opt.repeatable && !slot.is_empty() {
                return Err(UsageError(format!("--{name} given more than once")));
            }
            slot.push(value);
        } else {
            if inline.is_some() {
                return Err(UsageError(format!("--{name} takes no value")));
            }
            if parsed.flags.contains(&opt.name) {
                return Err(UsageError(format!("--{name} given more than once")));
            }
            parsed.flags.push(opt.name);
        }
    }
    Ok(parsed)
}

/// Parses a non-negative count given to an option.
pub(crate) fn count_value(name: &str, value: &str) -> Result<u64, UsageError> {
    value
        .parse()
        .map_err(|_| UsageError(format!("--{name} needs a whole number, not {value:?}")))
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::{Opt, UsageError, parse};

    const OPTS: &[Opt] = &[
        Opt::flag("json"),
        Opt::value("min-passed"),
        Opt::repeated("min-passed-in"),
    ];

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn unknown_option_is_usage_error() {
        assert_eq!(
            parse(&args(&["--jsno"]), OPTS).map(|_| ()),
            Err(UsageError("unknown option --jsno".to_owned()))
        );
    }

    #[test]
    fn missing_value_is_usage_error() {
        assert!(parse(&args(&["--min-passed"]), OPTS).is_err());
    }

    #[test]
    fn repeatable_options_collect() {
        let p = parse(
            &args(&["--min-passed-in", "a=1", "--min-passed-in=b=2", "x.log"]),
            OPTS,
        )
        .expect("parses");
        assert_eq!(p.values("min-passed-in"), ["a=1", "b=2"]);
        assert_eq!(p.positional, ["x.log"]);
    }

    #[test]
    fn single_value_option_given_twice_is_error() {
        assert!(parse(&args(&["--min-passed", "1", "--min-passed", "2"]), OPTS).is_err());
    }

    #[test]
    fn flag_with_value_is_error() {
        assert!(parse(&args(&["--json=yes"]), OPTS).is_err());
    }

    #[test]
    fn double_dash_starts_command() {
        let p = parse(
            &args(&["--json", "--", "cargo", "test", "--min-passed"]),
            OPTS,
        )
        .expect("parses");
        assert!(p.flag("json"));
        assert_eq!(
            p.command.as_deref(),
            Some(&args(&["cargo", "test", "--min-passed"])[..])
        );
    }
}
