//  src/mutate/table.rs

//! A mutation table is a TOML file in format version 1, and this module loads
//! and validates it.
//!
//! Top-level keys set defaults, and each `[[mutation]]` entry declares one edit
//! and the tests it must fail. Every problem in every table is collected and
//! reported together. An unknown key is an error that suggests the nearest
//! known key, because a mistyped `must_fial` must never silently mean that no
//! failures were expected.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use toml::Value;

use crate::count::REFUSED_FLAGS;
use crate::decode::decode;
use crate::reader::libtest::normalize_name;
use crate::split::split;

const TOP_KEYS: &[&str] = &[
    "version",
    "command",
    "min_passed",
    "env",
    "timeout",
    "timeout_multiplier",
    "timeout_floor",
    "root",
    "mutation",
];
const ENTRY_KEYS: &[&str] = &[
    "id",
    "what",
    "file",
    "find",
    "replace",
    "command",
    "must_fail",
    "may_fail",
    "env",
    "min_passed",
    "timeout",
    "expect",
];

// ===========================================================================
// Entries
// ===========================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Expect {
    /// The mutation must make the named tests fail, which is the default.
    TestsFail,
    /// The mutation must be rejected by the compiler.
    CompileError,
}

/// One validated `[[mutation]]` entry, with every default applied.
#[derive(Clone, Debug)]
pub(crate) struct Entry {
    pub table: PathBuf,
    pub id: String,
    pub what: String,
    /// An absolute path, either the table's `root` resolved against the table
    /// file's directory or `--root`.
    pub root: PathBuf,
    /// A path relative to `root`, as the table wrote it.
    pub file: PathBuf,
    pub finds: Vec<String>,
    pub replaces: Vec<String>,
    pub command: Vec<String>,
    /// The table's `env` with the entry's laid over it, sorted by name.
    pub env: Vec<(String, String)>,
    pub must_fail: Vec<String>,
    pub may_fail: Vec<String>,
    pub min_passed: u64,
    pub expect: Expect,
    /// The entry's `timeout`, or its table's when the entry sets none.
    pub timeout: Option<Duration>,
    /// The table's `timeout_multiplier` and `timeout_floor`, used for the
    /// timeout derived from the baseline.
    pub timeout_multiplier: Option<f64>,
    pub timeout_floor: Option<Duration>,
}

#[cfg(test)]
impl Entry {
    pub fn for_tests() -> Self {
        Self {
            table: PathBuf::from("t.toml"),
            id: String::new(),
            what: String::new(),
            root: PathBuf::from("."),
            file: PathBuf::from("src/lib.rs"),
            finds: Vec::new(),
            replaces: Vec::new(),
            command: vec!["cargo".to_owned(), "test".to_owned()],
            env: Vec::new(),
            must_fail: Vec::new(),
            may_fail: Vec::new(),
            min_passed: 1,
            expect: Expect::TestsFail,
            timeout: None,
            timeout_multiplier: None,
            timeout_floor: None,
        }
    }
}

/// A problem in a table, with where it is.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct LoadError {
    pub table: PathBuf,
    pub id: Option<String>,
    pub message: String,
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.id {
            Some(id) => write!(f, "{}: {id}: {}", self.table.display(), self.message),
            None => write!(f, "{}: {}", self.table.display(), self.message),
        }
    }
}

// ===========================================================================
// Reading values
// ===========================================================================

struct Ctx<'a> {
    table: &'a Path,
    id: Option<String>,
    errors: &'a mut Vec<LoadError>,
}

impl Ctx<'_> {
    fn err(&mut self, message: impl Into<String>) {
        self.errors.push(LoadError {
            table: self.table.to_owned(),
            id: self.id.clone(),
            message: message.into(),
        });
    }

    fn wrong(&mut self, key: &str, expected: &str, got: &Value) {
        self.err(format!(
            "`{key}` must be {expected}, not {}",
            got.type_str()
        ));
    }

    fn unknown_keys(&mut self, map: &toml::Table, known: &[&str]) {
        for key in map.keys().filter(|k| !known.contains(&k.as_str())) {
            match suggest(key, known) {
                Some(near) => self.err(format!("unknown key `{key}`; did you mean `{near}`?")),
                None => self.err(format!("unknown key `{key}`")),
            }
        }
    }

    fn string(&mut self, map: &toml::Table, key: &str) -> Option<String> {
        match map.get(key)? {
            Value::String(s) => Some(s.clone()),
            other => {
                self.wrong(key, "a string", other);
                None
            }
        }
    }

    fn strings(&mut self, map: &toml::Table, key: &str) -> Option<Vec<String>> {
        let value = map.get(key)?;
        let items = value
            .as_array()
            .and_then(|a| a.iter().map(|v| v.as_str().map(str::to_owned)).collect());
        if items.is_none() {
            self.wrong(key, "a list of strings", value);
        }
        items
    }

    /// A string or a list of strings, and whether it was a list.
    fn one_or_many(&mut self, map: &toml::Table, key: &str) -> Option<(Vec<String>, bool)> {
        match map.get(key)? {
            Value::String(s) => Some((vec![s.clone()], false)),
            Value::Array(_) => self.strings(map, key).map(|v| (v, true)),
            other => {
                self.wrong(key, "a string or a list of strings", other);
                None
            }
        }
    }

    fn count(&mut self, map: &toml::Table, key: &str) -> Option<u64> {
        let value = map.get(key)?;
        let n = value
            .as_integer()
            .and_then(|n| u64::try_from(n).ok())
            .filter(|n| *n >= 1);
        if n.is_none() {
            self.wrong(key, "a whole number of at least 1", value);
        }
        n
    }

    fn positive(&mut self, map: &toml::Table, key: &str) -> Option<f64> {
        let value = map.get(key)?;
        #[allow(clippy::cast_precision_loss)] // seconds; exact for any sensible value
        let n = value
            .as_float()
            .or_else(|| value.as_integer().map(|i| i as f64));
        let n = n.filter(|n| n.is_finite() && *n > 0.0);
        if n.is_none() {
            self.wrong(key, "a positive number", value);
        }
        n
    }

    fn seconds(&mut self, map: &toml::Table, key: &str) -> Option<Duration> {
        self.positive(map, key)
            .and_then(|s| Duration::try_from_secs_f64(s).ok())
    }

    fn command(&mut self, map: &toml::Table) -> Option<Vec<String>> {
        let args = match map.get("command")? {
            Value::String(s) => match split(s) {
                Ok(args) => args,
                Err(e) => {
                    self.err(format!("`command` cannot be split: {e}"));
                    return None;
                }
            },
            Value::Array(_) => self.strings(map, "command")?,
            other => {
                self.wrong("command", "a string or a list of strings", other);
                return None;
            }
        };
        if args.is_empty() {
            self.err("`command` is empty");
            return None;
        }
        Some(args)
    }

    fn env(&mut self, map: &toml::Table) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        let Some(value) = map.get("env") else {
            return out;
        };
        let Some(env) = value.as_table() else {
            self.wrong("env", "a table of strings", value);
            return out;
        };
        for (name, v) in env {
            match v {
                Value::String(s) => {
                    out.insert(name.clone(), s.clone());
                }
                other => self.err(format!(
                    "`env.{name}` must be a string (quote it), not {}",
                    other.type_str()
                )),
            }
        }
        out
    }
}

/// The known key nearest to `key`, if it is within two edits.
fn suggest<'a>(key: &str, known: &[&'a str]) -> Option<&'a str> {
    known
        .iter()
        .map(|k| (distance(key, k), *k))
        .filter(|(d, _)| *d <= 2)
        .min_by_key(|(d, _)| *d)
        .map(|(_, k)| k)
}

fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let next = (prev + usize::from(ca != *cb))
                .min(row[j] + 1)
                .min(row[j + 1] + 1);
            prev = row[j + 1];
            row[j + 1] = next;
        }
    }
    row[b.len()]
}

// ===========================================================================
// Loading
// ===========================================================================

/// `C:/dev/x/tests/..` becomes `C:/dev/x`, without touching the file system
/// (so no `\\?\` prefix on Windows, and symbolic links are left alone).
fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push(part);
                }
            }
            other => out.push(other),
        }
    }
    out
}

/// Table-wide settings that entries inherit.
struct Defaults {
    root: PathBuf,
    command: Option<Vec<String>>,
    min_passed: Option<u64>,
    env: BTreeMap<String, String>,
    timeout: Option<Duration>,
    timeout_multiplier: Option<f64>,
    timeout_floor: Option<Duration>,
}

/// Loads and validates every table. Returns every entry, or every problem.
pub(crate) fn load(
    paths: &[PathBuf],
    root_override: Option<&Path>,
) -> Result<Vec<Entry>, Vec<LoadError>> {
    let mut errors = Vec::new();
    let mut entries = Vec::new();
    for path in paths {
        load_table(path, root_override, &mut entries, &mut errors);
    }
    let mut first: HashMap<&str, &Path> = HashMap::new();
    for entry in &entries {
        if let Some(table) = first.get(entry.id.as_str()) {
            let message = if *table == entry.table.as_path() {
                "the id is used twice in this table".to_owned()
            } else {
                format!("the id is also used in {}", table.display())
            };
            errors.push(LoadError {
                table: entry.table.clone(),
                id: Some(entry.id.clone()),
                message,
            });
        } else {
            first.insert(&entry.id, &entry.table);
        }
    }
    if errors.is_empty() {
        Ok(entries)
    } else {
        Err(errors)
    }
}

fn read_doc(path: &Path, ctx: &mut Ctx<'_>) -> Option<toml::Table> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) => {
            ctx.err(format!("cannot read the table: {e}"));
            return None;
        }
    };
    let (text, lossy) = decode(&bytes);
    if lossy {
        ctx.err("the table is not valid UTF-8 or UTF-16 text");
        return None;
    }
    match text.parse::<toml::Table>() {
        Ok(doc) => Some(doc),
        Err(e) => {
            ctx.err(format!(
                "the table is not valid TOML: {}",
                e.to_string().trim_end()
            ));
            None
        }
    }
}

fn load_table(
    path: &Path,
    root_override: Option<&Path>,
    entries: &mut Vec<Entry>,
    errors: &mut Vec<LoadError>,
) {
    let mut ctx = Ctx {
        table: path,
        id: None,
        errors,
    };
    let Some(doc) = read_doc(path, &mut ctx) else {
        return;
    };
    ctx.unknown_keys(&doc, TOP_KEYS);
    match doc.get("version") {
        None => ctx.err("`version` is missing: add `version = 1` at the top of the table"),
        Some(Value::Integer(1)) => {}
        Some(Value::Integer(n)) => ctx.err(format!(
            "`version = {n}` is not supported; this ostiarius reads version 1"
        )),
        Some(other) => ctx.wrong("version", "the whole number 1", other),
    }
    let table_dir = std::path::absolute(path)
        .ok()
        .and_then(|p| p.parent().map(Path::to_owned))
        .unwrap_or_default();
    let root = match root_override {
        Some(root) => root.to_owned(),
        None => table_dir.join(ctx.string(&doc, "root").unwrap_or_else(|| ".".to_owned())),
    };
    let root = lexical(&std::path::absolute(&root).unwrap_or(root));
    if !root.is_dir() {
        ctx.err(format!("the root {} is not a directory", root.display()));
        return;
    }
    let defaults = Defaults {
        root,
        command: ctx.command(&doc),
        min_passed: ctx.count(&doc, "min_passed"),
        env: ctx.env(&doc),
        timeout: ctx.seconds(&doc, "timeout"),
        timeout_multiplier: ctx.positive(&doc, "timeout_multiplier"),
        timeout_floor: ctx.seconds(&doc, "timeout_floor"),
    };
    let items = match doc.get("mutation") {
        None => {
            ctx.err("the table has no [[mutation]] entries");
            return;
        }
        Some(Value::Array(items)) if items.is_empty() => {
            ctx.err("the table has no [[mutation]] entries");
            return;
        }
        Some(Value::Array(items)) => items,
        Some(other) => {
            ctx.wrong("mutation", "a list of tables ([[mutation]])", other);
            return;
        }
    };
    for (n, item) in items.iter().enumerate() {
        let Some(map) = item.as_table() else {
            ctx.err(format!("mutation {} is not a table", n + 1));
            continue;
        };
        let id = map
            .get("id")
            .and_then(Value::as_str)
            .map_or_else(|| format!("(mutation {})", n + 1), str::to_owned);
        let mut entry_ctx = Ctx {
            table: path,
            id: Some(id),
            errors: &mut *ctx.errors,
        };
        if let Some(entry) = load_entry(map, &defaults, &mut entry_ctx) {
            entries.push(entry);
        }
    }
}

fn load_entry(map: &toml::Table, d: &Defaults, ctx: &mut Ctx<'_>) -> Option<Entry> {
    let before = ctx.errors.len();
    ctx.unknown_keys(map, ENTRY_KEYS);
    let id = ctx.string(map, "id");
    match &id {
        None if !map.contains_key("id") => ctx.err("lacks `id`"),
        Some(id) if id.trim().is_empty() => ctx.err("`id` is empty"),
        _ => {}
    }
    let file = entry_file(map, &d.root, ctx);
    let (finds, replaces) = entry_edit(map, ctx);
    let command = ctx.command(map).or_else(|| d.command.clone());
    match &command {
        None if !map.contains_key("command") => {
            ctx.err("no command: set `command` at the top of the table or in this entry");
        }
        Some(args) => {
            if let Some(flag) = args.iter().find(|a| REFUSED_FLAGS.contains(&a.as_str())) {
                ctx.err(format!("the command uses {flag}, which prints tests' output among the result lines ostiarius reads"));
            }
        }
        None => {}
    }
    let expect = match ctx.string(map, "expect").as_deref() {
        None | Some("tests-fail") => Expect::TestsFail,
        Some("compile-error") => Expect::CompileError,
        Some(other) => {
            ctx.err(format!(
                "`expect` must be \"tests-fail\" or \"compile-error\", not {other:?}"
            ));
            Expect::TestsFail
        }
    };
    let (must_fail, may_fail) = entry_tests(map, expect, ctx);
    let mut env = d.env.clone();
    env.extend(ctx.env(map));
    let entry = Entry {
        table: ctx.table.to_owned(),
        id: id.unwrap_or_default(),
        what: ctx.string(map, "what").unwrap_or_default(),
        root: d.root.clone(),
        file: file.unwrap_or_default(),
        finds,
        replaces,
        command: command.unwrap_or_default(),
        env: env.into_iter().collect(),
        must_fail,
        may_fail,
        min_passed: ctx.count(map, "min_passed").or(d.min_passed).unwrap_or(1),
        expect,
        timeout: ctx.seconds(map, "timeout").or(d.timeout),
        timeout_multiplier: d.timeout_multiplier,
        timeout_floor: d.timeout_floor,
    };
    (ctx.errors.len() == before).then_some(entry)
}

fn entry_file(map: &toml::Table, root: &Path, ctx: &mut Ctx<'_>) -> Option<PathBuf> {
    let Some(file) = ctx.string(map, "file") else {
        if !map.contains_key("file") {
            ctx.err("lacks `file`");
        }
        return None;
    };
    let path = PathBuf::from(&file);
    if path.has_root() || path.is_absolute() {
        ctx.err("`file` must be relative to the root");
        return None;
    }
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        ctx.err("`file` may not leave its root (no `..`)");
        return None;
    }
    if !root.join(&path).is_file() {
        ctx.err(format!(
            "`file` {file} does not exist under {}",
            root.display()
        ));
        return None;
    }
    Some(path)
}

fn entry_edit(map: &toml::Table, ctx: &mut Ctx<'_>) -> (Vec<String>, Vec<String>) {
    for key in ["find", "replace"] {
        if !map.contains_key(key) {
            ctx.err(format!("lacks `{key}`"));
        }
    }
    let (Some((finds, finds_list)), Some((replaces, replaces_list))) = (
        ctx.one_or_many(map, "find"),
        ctx.one_or_many(map, "replace"),
    ) else {
        return (Vec::new(), Vec::new());
    };
    if finds_list != replaces_list {
        ctx.err("`find` and `replace` must both be strings or both be lists");
    } else if finds.len() != replaces.len() {
        ctx.err(format!(
            "`find` has {} items but `replace` has {}",
            finds.len(),
            replaces.len()
        ));
    } else if finds.is_empty() {
        ctx.err("`find` is empty");
    }
    for (k, find) in finds.iter().enumerate() {
        if find.is_empty() {
            ctx.err(if finds_list {
                format!("find {} is empty", k + 1)
            } else {
                "`find` is empty".to_owned()
            });
        }
    }
    (finds, replaces)
}

fn entry_tests(map: &toml::Table, expect: Expect, ctx: &mut Ctx<'_>) -> (Vec<String>, Vec<String>) {
    let must = ctx.strings(map, "must_fail");
    let may = ctx.strings(map, "may_fail");
    match expect {
        Expect::TestsFail => match &must {
            None if !map.contains_key("must_fail") => ctx.err("lacks `must_fail`: name the tests the mutation must fail"),
            Some(names) if names.is_empty() => ctx.err(
                "`must_fail` is empty: every entry must name at least one test that fails deterministically (a randomized test that only sometimes fails goes in `may_fail`)",
            ),
            _ => {}
        },
        Expect::CompileError => {
            if map.contains_key("may_fail") {
                ctx.err("`may_fail` has no meaning with expect = \"compile-error\"");
            }
        }
    }
    let normalize = |names: Option<Vec<String>>| {
        let mut names: Vec<String> = names
            .unwrap_or_default()
            .iter()
            .map(|n| normalize_name(n))
            .collect();
        names.sort();
        names.dedup();
        names
    };
    (normalize(must), normalize(may))
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::{Expect, LoadError, load};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    static NEXT: AtomicUsize = AtomicUsize::new(0);

    /// A temporary directory with `src/lib.rs`, and the tables written in it.
    fn dir_with(tables: &[(&str, &str)]) -> (PathBuf, Vec<PathBuf>) {
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("ostiarius-table-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("src")).expect("temp dir");
        fs::write(dir.join("src/lib.rs"), "fn f() {}\n").expect("lib.rs");
        let paths = tables
            .iter()
            .map(|(name, text)| {
                let path = dir.join(name);
                fs::write(&path, text).expect("table");
                path
            })
            .collect();
        (dir, paths)
    }

    const ENTRY: &str = r#"
[[mutation]]
id = "m1"
file = "src/lib.rs"
find = "fn f"
replace = "fn g"
must_fail = ["tests::a"]
"#;

    fn load_text(text: &str) -> Result<Vec<super::Entry>, Vec<LoadError>> {
        let (dir, paths) = dir_with(&[("t.toml", text)]);
        let out = load(&paths, None);
        let _ = fs::remove_dir_all(dir);
        out
    }

    fn messages(text: &str) -> Vec<String> {
        load_text(text)
            .map(|_| ())
            .expect_err("expected errors")
            .into_iter()
            .map(|e| e.message)
            .collect()
    }

    fn has(text: &str, part: &str) {
        let msgs = messages(text);
        assert!(
            msgs.iter().any(|m| m.contains(part)),
            "no message contains {part:?}: {msgs:#?}"
        );
    }

    #[test]
    fn minimal_table_loads() {
        let entries = load_text(&format!(
            "version = 1\ncommand = \"cargo test -p demo\"\n{ENTRY}"
        ))
        .expect("loads");
        assert_eq!(entries.len(), 1);
        let e = &entries[0];
        assert_eq!(
            (e.id.as_str(), e.min_passed, e.expect),
            ("m1", 1, Expect::TestsFail)
        );
        assert_eq!(e.command, ["cargo", "test", "-p", "demo"]);
        assert_eq!(e.must_fail, ["tests::a"]);
        assert!(e.root.is_absolute());
    }

    #[test]
    fn version_missing() {
        has(&format!("command = \"x\"\n{ENTRY}"), "`version` is missing");
    }

    #[test]
    fn version_two_refused() {
        has(
            &format!("version = 2\ncommand = \"x\"\n{ENTRY}"),
            "`version = 2` is not supported",
        );
    }

    #[test]
    fn unknown_key_suggests() {
        let text =
            format!("version = 1\ncommand = \"x\"\n{ENTRY}").replace("must_fail", "must_fial");
        has(&text, "unknown key `must_fial`; did you mean `must_fail`?");
        has(
            "version = 1\ncomand = \"x\"\n[[mutation]]\nid = \"a\"",
            "unknown key `comand`; did you mean `command`?",
        );
        has(
            &format!("version = 1\ncommand = \"x\"\nzebra = 1\n{ENTRY}"),
            "unknown key `zebra`",
        );
    }

    #[test]
    fn wrong_type_named() {
        has(
            &format!("version = 1\ncommand = \"x\"\nmin_passed = \"3\"\n{ENTRY}"),
            "`min_passed` must be a whole number of at least 1, not string",
        );
        has(
            &format!("version = 1\ncommand = 3\n{ENTRY}"),
            "`command` must be a string or a list of strings, not integer",
        );
        has(
            &format!("version = 1\ncommand = \"x\"\nenv = {{ N = 5 }}\n{ENTRY}"),
            "`env.N` must be a string (quote it), not integer",
        );
    }

    #[test]
    fn find_replace_length_mismatch() {
        let text = format!("version = 1\ncommand = \"x\"\n{ENTRY}")
            .replace("find = \"fn f\"", "find = [\"fn\", \"f\"]")
            .replace("replace = \"fn g\"", "replace = [\"x\"]");
        has(&text, "`find` has 2 items but `replace` has 1");
        let mixed = format!("version = 1\ncommand = \"x\"\n{ENTRY}")
            .replace("replace = \"fn g\"", "replace = [\"x\"]");
        has(
            &mixed,
            "`find` and `replace` must both be strings or both be lists",
        );
    }

    #[test]
    fn empty_must_fail_refused() {
        let text = format!("version = 1\ncommand = \"x\"\n{ENTRY}").replace("[\"tests::a\"]", "[]");
        has(&text, "`must_fail` is empty");
        let text = format!("version = 1\ncommand = \"x\"\n{ENTRY}")
            .replace("must_fail = [\"tests::a\"]", "");
        has(&text, "lacks `must_fail`");
    }

    #[test]
    fn compile_error_allows_empty_must_fail() {
        let text = format!("version = 1\ncommand = \"x\"\n{ENTRY}expect = \"compile-error\"\n")
            .replace("must_fail = [\"tests::a\"]", "");
        let entries = load_text(&text).expect("loads");
        assert_eq!(entries[0].expect, Expect::CompileError);
        has(
            &format!("{text}may_fail = [\"b\"]\n"),
            "`may_fail` has no meaning",
        );
        has(
            &text.replace("compile-error", "compile_error"),
            "`expect` must be \"tests-fail\" or \"compile-error\"",
        );
    }

    #[test]
    fn duplicate_id_across_tables() {
        let text = format!("version = 1\ncommand = \"x\"\n{ENTRY}");
        let (dir, paths) = dir_with(&[("a.toml", &text), ("b.toml", &text)]);
        let errors = load(&paths, None).map(|_| ()).expect_err("duplicate");
        let _ = fs::remove_dir_all(dir);
        assert_eq!(errors.len(), 1);
        assert!(
            errors[0].message.starts_with("the id is also used in "),
            "{}",
            errors[0]
        );
        has(
            &format!("version = 1\ncommand = \"x\"\n{ENTRY}{ENTRY}"),
            "the id is used twice in this table",
        );
    }

    #[test]
    fn command_list_kept() {
        let entries = load_text(&format!(
            "version = 1\ncommand = [\"cargo\", \"test\", \"C:\\\\dev\\\\x\"]\n{ENTRY}"
        ))
        .expect("loads");
        assert_eq!(entries[0].command, ["cargo", "test", r"C:\dev\x"]);
    }

    #[test]
    fn command_string_split() {
        let entries = load_text(&format!(
            "version = 1\ncommand = \"cargo test -- 'a b'\"\n{ENTRY}"
        ))
        .expect("loads");
        assert_eq!(entries[0].command, ["cargo", "test", "--", "a b"]);
    }

    #[test]
    fn unterminated_quote_in_command() {
        has(
            &format!("version = 1\ncommand = \"cargo 'test\"\n{ENTRY}"),
            "`command` cannot be split: the quote ' is never closed",
        );
    }

    #[test]
    fn nocapture_refused() {
        has(
            &format!("version = 1\ncommand = \"cargo test -- --nocapture\"\n{ENTRY}"),
            "the command uses --nocapture",
        );
    }

    #[test]
    fn missing_command_refused() {
        has(
            &format!("version = 1\n{ENTRY}"),
            "no command: set `command`",
        );
    }

    #[test]
    fn env_entry_overrides_table() {
        let text = format!(
            "version = 1\ncommand = \"x\"\nenv = {{ A = \"1\", B = \"2\" }}\n{ENTRY}env = {{ B = \"3\" }}\n"
        );
        let entries = load_text(&text).expect("loads");
        assert_eq!(
            entries[0].env,
            [
                ("A".to_owned(), "1".to_owned()),
                ("B".to_owned(), "3".to_owned())
            ]
        );
    }

    #[test]
    fn min_passed_and_timeout_precedence() {
        let text = format!(
            "version = 1\ncommand = \"x\"\nmin_passed = 5\ntimeout = 30\n{ENTRY}{}",
            ENTRY.replace("\"m1\"", "\"m2\"") + "min_passed = 2\ntimeout = 1.5\n"
        );
        let entries = load_text(&text).expect("loads");
        assert_eq!((entries[0].min_passed, entries[1].min_passed), (5, 2));
        assert_eq!(
            (entries[0].timeout, entries[1].timeout),
            (
                Some(Duration::from_secs(30)),
                Some(Duration::from_millis(1500))
            )
        );
        has(
            &format!("version = 1\ncommand = \"x\"\ntimeout = 0\n{ENTRY}"),
            "`timeout` must be a positive number",
        );
    }

    #[test]
    fn root_relative_to_table_file() {
        let (dir, _) = dir_with(&[]);
        fs::create_dir_all(dir.join("tables")).expect("dir");
        let table = dir.join("tables/t.toml");
        fs::write(
            &table,
            format!("version = 1\nroot = \"..\"\ncommand = \"x\"\n{ENTRY}"),
        )
        .expect("table");
        let entries = load(&[table], None).expect("loads");
        assert_eq!(
            fs::canonicalize(&entries[0].root).expect("root"),
            fs::canonicalize(&dir).expect("dir")
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn root_shown_without_dot_dot() {
        assert_eq!(
            super::lexical(Path::new("/a/b/../c/./d")),
            Path::new("/a/c/d")
        );
    }

    #[test]
    fn root_override() {
        let (dir, paths) = dir_with(&[(
            "t.toml",
            &format!("version = 1\nroot = \"nowhere\"\ncommand = \"x\"\n{ENTRY}"),
        )]);
        assert!(load(&paths, None).is_err());
        assert!(load(&paths, Some(&dir)).is_ok());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn file_escaping_root_refused() {
        let base = format!("version = 1\ncommand = \"x\"\n{ENTRY}");
        has(
            &base.replace("src/lib.rs", "../lib.rs"),
            "`file` may not leave its root",
        );
        has(
            &base.replace("src/lib.rs", "src/gone.rs"),
            "`file` src/gone.rs does not exist under",
        );
        let absolute = if cfg!(windows) { r"C:\\x.rs" } else { "/x.rs" };
        has(
            &base.replace("src/lib.rs", absolute),
            "`file` must be relative to the root",
        );
    }

    #[test]
    fn all_errors_reported_together() {
        let text = "command = 3\nzebra = 1\n[[mutation]]\nid = \"a\"\n[[mutation]]\nfile = \"x\"\n";
        let msgs = messages(text);
        assert!(msgs.len() >= 6, "{msgs:#?}");
    }

    #[test]
    fn errors_name_table_and_entry() {
        let (dir, paths) = dir_with(&[(
            "t.toml",
            &format!("version = 1\ncommand = \"x\"\n{ENTRY}").replace("[\"tests::a\"]", "[]"),
        )]);
        let errors = load(&paths, None).map(|_| ()).expect_err("errors");
        let _ = fs::remove_dir_all(&dir);
        let shown = errors[0].to_string();
        assert!(
            shown.contains(&format!("{}: m1: ", Path::new(&paths[0]).display())),
            "{shown}"
        );
    }

    #[test]
    fn utf16_table_is_read() {
        let text = format!("version = 1\ncommand = \"x\"\n{ENTRY}");
        let mut bytes = vec![0xFF, 0xFE];
        for unit in text.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        let (dir, _) = dir_with(&[]);
        let path = dir.join("t.toml");
        fs::write(&path, bytes).expect("write");
        assert!(load(&[path], None).is_ok());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn test_names_normalized() {
        let text = format!("version = 1\ncommand = \"x\"\n{ENTRY}").replace(
            "[\"tests::a\"]",
            r#"['src\lib.rs - f (line 3)', "tests::a"]"#,
        );
        let entries = load_text(&text).expect("loads");
        assert_eq!(entries[0].must_fail, ["src/lib.rs - f", "tests::a"]);
    }
}
