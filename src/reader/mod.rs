//  src/reader/mod.rs

//! Test-runner output is read into one shape that both subcommands share.
//!
//! Only libtest's human-readable output is read today. `Run` is the seam where
//! another reader would attach, since a reader for a different runner's output
//! produces the same `Run` and nothing that consumes it needs to change.

pub(crate) mod libtest;

// ===========================================================================
// The shared shape
// ===========================================================================

/// How one test ended, as its `test <name> ... <outcome>` line reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Ok,
    Failed,
    Ignored,
}

/// One test binary's results, meaning one `test result:` line and the test
/// lines that preceded it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Binary {
    /// The binary's label in the form `<target> (<stem>)`, for example
    /// `unittests src/lib.rs (demo)`, or `(unlabeled N)` when no Cargo header
    /// preceded the result.
    pub label: String,
    pub passed: u64,
    pub failed: u64,
    pub ignored: u64,
    /// Every test line outside captured output, with its name normalized.
    pub tests: Vec<(String, Outcome)>,
}

/// Everything read from one log.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Run {
    pub binaries: Vec<Binary>,
    /// Why the log cannot be trusted, when it cannot.
    pub suspect: Option<String>,
}

impl Run {
    pub fn passed(&self) -> u64 {
        self.binaries.iter().map(|b| b.passed).sum()
    }

    pub fn failed(&self) -> u64 {
        self.binaries.iter().map(|b| b.failed).sum()
    }

    pub fn ignored(&self) -> u64 {
        self.binaries.iter().map(|b| b.ignored).sum()
    }

    /// The names of every failing test, sorted and without duplicates.
    pub fn failing(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .binaries
            .iter()
            .flat_map(|b| b.tests.iter())
            .filter(|(_, outcome)| *outcome == Outcome::Failed)
            .map(|(name, _)| name.clone())
            .collect();
        names.sort();
        names.dedup();
        names
    }
}

/// A test runner's output format.
pub(crate) trait Reader {
    /// Reads a run's output.
    fn parse(&self, text: &str) -> Run;
    /// Reads the output of a listing that names tests without running them.
    fn parse_list(&self, text: &str) -> Vec<String>;
}
