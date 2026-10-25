# The mutation table format, version 1

A mutation table states a claim about a test suite and gives `ostiarius mutate` what it needs to test that claim. Each entry names one edit that breaks an argument the tests are supposed to rest on, and the tests that must fail when that argument is broken. A run applies every edit in turn, checks that exactly the named tests fail, and restores each file byte for byte. This document is the complete reference for the format. Every key, default, rule and message the tool can produce is described here, and a test in the crate's own suite fails if a message the code can print is missing from section 9.

## 1. A complete table

The table below comes from `examples/intervals.mutations.toml`, shortened to two entries. The crate it targets has one function, `overlaps`, which decides whether two closed intervals share a point.

```toml
version = 1                                   # required, and must be 1
root = "intervals"                            # relative to this file; default "."
command = ["cargo", "test", "--lib"]          # run for every entry unless an entry sets its own
min_passed = 5                                # the baseline and the restored run must pass at least this many

[[mutation]]
id = "open-interval"                          # unique across every table in the run
what = "the closed comparison made open"      # shown in the output
file = "src/interval.rs"                      # relative to the root
find = "a.lo <= b.hi && b.lo <= a.hi"         # must occur exactly once in the file
replace = "a.lo < b.hi && b.lo < a.hi"
must_fail = ["interval::tests::touching_intervals_overlap"]

[[mutation]]
id = "off-by-one"
what = "the upper bound widened by one"
file = "src/interval.rs"
find = "b.lo <= a.hi"
replace = "b.lo <= a.hi + 1"
must_fail = [
    "interval::tests::adjacent_intervals_do_not",
    "interval::tests::overlap_is_symmetric",
]
```

Running it prints one line for the baseline, one per entry and one for the restored run.

```text
root: C:\dev\ostiarius\examples\intervals
ok   baseline: 5 passed in 0.5 s (entry timeout up to 60 s): cargo test --lib
ok   open-interval            the closed comparison made open: fails touching_intervals_overlap
ok   off-by-one               the upper bound widened by one: fails adjacent_intervals_do_not, overlap_is_symmetric
ok   restored: 5 passed (need 5): cargo test --lib
```

## 2. Top-level keys

Top-level keys set the table's defaults. Any key not listed here is an error, and the tool suggests the nearest known key when one is within two edits.

| Key | Type | Default | Meaning |
|---|---|---|---|
| `version` | integer | required | The format version. It must be `1`. |
| `command` | string, or list of strings | none | The command each entry runs unless it sets its own. A list is passed as written, while a string is split by POSIX quoting rules (section 5.5). |
| `min_passed` | whole number of at least 1 | 1 | The fewest tests the baseline and the restored run must pass. |
| `env` | table of strings | empty | Environment variables added for every run. Values must be strings, so a number is quoted (`CASES = "500"`). |
| `timeout` | positive number of seconds | derived | The limit for each run of an entry that sets none of its own (section 8.2). |
| `timeout_multiplier` | positive number | 5 | Used for the derived timeout. |
| `timeout_floor` | positive number of seconds | 60 | Used for the derived timeout. |
| `root` | string path | the table file's folder | The folder that `file` paths are relative to and that commands run in. A relative `root` is resolved against the table file's folder. `--root` replaces every table's root. |
| `mutation` | list of tables | required | The entries, written as `[[mutation]]` sections. There must be at least one. |

## 3. Entry keys

Each `[[mutation]]` section is one entry. An entry inherits `command`, `min_passed`, `env` and `timeout` from the top of its table unless it sets them itself.

| Key | Type | Default | Meaning |
|---|---|---|---|
| `id` | non-empty string | required | Names the entry in output and logs, and selects it with `--id`. It must be unique across every table loaded in one run. |
| `what` | string | `""` | A short description, shown after the id. |
| `file` | string path | required | The file to edit, relative to the root. It must exist and may not be absolute or contain `..`. |
| `find` | string, or list of strings | required | The text to replace. A list applies in order (section 5.1). |
| `replace` | string, or list of strings | required | The replacement for each find. It must have the same shape as `find`, a string for a string and a list of the same length for a list. |
| `must_fail` | list of test names | required | The tests the mutation must fail. It may not be empty (section 7.1). |
| `may_fail` | list of test names | empty | Tests that are allowed, but not required, to fail (section 7.1). |
| `command` | string, or list of strings | the table's | Overrides the table's command. |
| `env` | table of strings | empty | Added over the table's `env`, so an entry's value wins for the same name. |
| `min_passed` | whole number of at least 1 | the table's | Overrides the table's floor for this entry's restored run. |
| `timeout` | positive number of seconds | the table's | Overrides the table's timeout. |
| `expect` | `"tests-fail"` or `"compile-error"` | `"tests-fail"` | What the mutation must do (section 7.2). |

## 4. Precedence

Settings come from more than one place, and the closer setting always wins. For `timeout`, an entry's own value comes first, then its table's value, then `--timeout` on the command line, and last the timeout derived from the baseline. For `env`, the entry's variables are laid over the table's, so both apply and the entry's value wins for a name they share. For `command` and `min_passed`, an entry's value replaces its table's, and `min_passed` falls back to 1 when neither sets it. For `root`, `--root` replaces every table's root, and otherwise each table's root is resolved against its own file's folder.

## 5. Matching rules

### 5.1 Each find occurs exactly once, in order

An edit means exactly one thing only if every find matches exactly one place. Each find must occur exactly once, and when an entry has several finds they apply in order, each counted in the text the earlier replacements left rather than in the original. A replacement that creates or removes an occurrence of a later find is therefore caught before anything is touched, rather than applying twice or not at all. A find that occurs more than once needs more of its surrounding text included until it is unique.

### 5.2 A find that no longer matches

When a find occurs nowhere, the code the entry targets has changed since the table was written. The tool reports this as a changed claim rather than a typing mistake, because the argument the entry tested may no longer hold in the new code. The entry's claim should be examined against the current code before its `find` is updated.

### 5.3 Line endings

A find that spans lines is written with `\n`. In a file whose line endings are all CRLF, as in a Windows checkout with `core.autocrlf`, the find matches as `\r\n` and its replacement is written with `\r\n`, so the edit keeps the file's own endings. In a file that mixes CRLF and LF, which ending a multi-line find should match is ambiguous, and the edit is refused. A single-line find is unaffected by line endings. Restoring always writes back the original bytes, whatever the endings.

### 5.4 Edits that change nothing, and text encodings

An edit whose replacements equal its finds is refused, since running the tests on an unchanged file could only report that nothing caught the mutation. Target files must be UTF-8. Tables themselves may be UTF-8 or UTF-16 with a byte-order mark, so a table saved by Windows PowerShell's `Out-File` loads correctly.

### 5.5 Writing code in TOML strings

TOML has two kinds of string. A basic string (`"..."`) treats a backslash as an escape, so `"\n"` is a line break and a literal backslash must be written `\\`. A literal string (`'...'`) keeps every character as written, which makes it the better choice for code containing backslashes or double quotes. A literal string cannot contain a single quote and cannot span lines unless it is written with three quotes (`'''...'''`).

```toml
find = 'path.replace("\\", "/")'              # literal string, matches  path.replace("\\", "/")
find = "path.replace(\"\\\\\", \"/\")"          # the same text as a basic string
find = "let k = 3;\n    x * k"                # two lines, matched as CRLF in a CRLF file
```

A command written as a string is split by POSIX quoting rules without any expansion. Unquoted whitespace separates arguments, single quotes keep their contents literally, and backslashes outside single quotes act as escapes. Those rules remove the backslashes from an unquoted Windows path, which is why a list of arguments is the portable form.

## 6. Test names

Tests are named by their full path as `cargo test` prints them, for example `interval::tests::touching_intervals_overlap`. A doctest prints with a line number, as in `src/lib.rs - overlaps (line 12)`, but the line number moves with every edit above it. Doctests are therefore named without the `(line N)` suffix and with forward slashes, so `src\lib.rs - overlaps (line 12)` on Windows and `src/lib.rs - overlaps (line 12)` elsewhere are both written `src/lib.rs - overlaps`. Names in tables are normalized the same way, so a table may also use the printed form. A test's captured output is never read as results, so a failing test that prints a line shaped like `test other ... FAILED` changes nothing.

## 7. What an entry expects

### 7.1 `must_fail` and `may_fail`

An entry passes only if every test in `must_fail` failed and every other failing test is listed in `may_fail`. `must_fail` may not be empty, because an entry with no required failure would pass on any mutation that broke something, which proves nothing about the argument the entry names. `may_fail` exists for randomized tests, such as property tests, whose cases are drawn afresh on each run and that catch a mutation on some runs but not others. A test that fails deterministically belongs in `must_fail`. Every entry still needs at least one deterministic test there, even when it lists randomized tests in `may_fail`.

### 7.2 `expect = "compile-error"`

Some arguments are enforced by the type checker rather than by tests. An entry with `expect = "compile-error"` passes only if the mutated tree fails to build with a compiler error. Its `must_fail` may be empty or absent, and `may_fail` has no meaning for it and is refused.

## 8. The run and its verdicts

### 8.1 Order

A run loads and validates every table and reports all problems together. It then refuses a tree that may already hold a mutation. That happens when saved copies remain from a run that was stopped outright, or when a target file has uncommitted changes in a git work tree, since a leftover mutation looks exactly like an uncommitted change. `--allow-dirty` skips the second check. Outside a git work tree that check is skipped with a note. The refusals come before the edit check because a leftover mutation makes its entry's find look stale, and the edit check would then name the wrong cause.

The run then checks that every edit applies, and runs the baseline. Each distinct combination of command, environment and root runs once on the unmutated tree, which must pass with at least the largest `min_passed` among its entries. Before the baseline, each target file's modification time is set to now, so the measured time includes the same incremental build a mutation pays for. Each entry then runs. Its file is saved, edited and its command run, and the file is restored and its bytes confirmed, whatever ended the command. Last, each distinct command runs once more on the restored tree.

### 8.2 Timeouts

An entry's timeout is the first of these that is set: its own `timeout`, its table's `timeout`, `--timeout`, or a timeout derived from the baseline, `max(timeout_floor, timeout_multiplier × baseline time)`. The baseline line reports the result. A derived timeout is generous by design, since its only purpose is to turn a hang into a failure. A run past its timeout has its whole process tree killed, including any test binary still running, and the file is restored.

### 8.3 Verdict lines

| Line | Meaning |
|---|---|
| `ok   <id> <what>: fails <tests>` | Every test in `must_fail` failed and nothing else did, or the others are listed in `may_fail`, which the line reports as `; also <tests> (may fail)`. |
| `ok   <id> <what>: rejected by the compiler` | An `expect = "compile-error"` entry failed to build, as required. |
| `FAIL <id> <what>: ...` | The entry did not fail as declared. The rest of the line says how (section 9.4). |
| `TIMEOUT <id> <what>: ...` | The command outlived its timeout. This counts as a failure, since a hang is not a named test failing. |
| `ok   restored: <n> passed (need <m>): <command>` | The restored tree passed with at least the required number of tests. |

### 8.4 Exit codes

| Code | Meaning |
|---|---|
| 0 | Every entry and every restored run passed. |
| 1 | An entry or a restored run failed or timed out. |
| 2 | The run was refused, a table or an edit has a problem, or a file could not be restored. |
| 3 | A usage error on the command line. |
| 130 | Ctrl-C stopped the run, after the file being mutated was restored. |

## 9. Messages

Each message appears below as the tool prints it, with placeholders in angle brackets. Every loader message is prefixed with the table's path, and with the entry's id when the problem is inside an entry.

### 9.1 Loading a table

| Message | Cause and fix |
|---|---|
| `cannot read the table: <reason>` | The file could not be opened. Check the path. |
| `the table is not valid UTF-8 or UTF-16 text` | The file holds bytes that are neither encoding. Save it as UTF-8. |
| `the table is not valid TOML: <parser message>` | A syntax error, with its line and column. |
| ``unknown key `<key>`; did you mean `<near>`?`` | A misspelled key. Correct it as suggested. |
| ``unknown key `<key>` `` | A key the format does not have, with no near match. Remove it. |
| `` `<key>` must be <type>, not <found type> `` | The value has the wrong type. The expected type is one of a string, a list of strings, a string or a list of strings, a whole number of at least 1, a positive number, a table of strings, the whole number 1, or a list of tables ([[mutation]]). |
| `` `env.<name>` must be a string (quote it), not <type> `` | An environment value written as a number or boolean. Quote it. |
| `` `version` is missing: add `version = 1` at the top of the table `` | The table has no `version` key. |
| `` `version = <n>` is not supported; this ostiarius reads version 1 `` | The table was written for another format version. |
| `the root <path> is not a directory` | `root` names a folder that does not exist. |
| `the table has no [[mutation]] entries` | The table declares no entries. |
| `mutation <n> is not a table` | An entry was written in a form other than a `[[mutation]]` section. |
| `the id is used twice in this table` | Two entries in one table share an id. |
| `the id is also used in <table>` | An entry in another table loaded by the same run has this id. |

### 9.2 Entry keys

| Message | Cause and fix |
|---|---|
| ``lacks `id` ``, ``lacks `file` ``, ``lacks `find` `` or ``lacks `replace` `` | A required key is missing. |
| `` `id` is empty `` | The id is blank. |
| ``no command: set `command` at the top of the table or in this entry`` | Neither the table nor the entry has a command. |
| `` `command` cannot be split: <reason> `` | A command string has an unclosed quote or ends with a lone backslash. Close the quote, or give the command as a list. |
| `` `command` is empty `` | The command has no arguments. |
| `the command uses <flag>, which prints tests' output among the result lines ostiarius reads` | `--nocapture` or `--show-output` would mix test output with results. Remove the flag. |
| `` `expect` must be "tests-fail" or "compile-error", not <value> `` | An unknown expectation. |
| `` `file` must be relative to the root `` | `file` is an absolute path. |
| `` `file` may not leave its root (no `..`) `` | `file` contains `..`. |
| `` `file` <file> does not exist under <root> `` | The target file is missing, or `root` is wrong. |
| `` `find` and `replace` must both be strings or both be lists `` | One is a string and the other a list. |
| `` `find` has <n> items but `replace` has <m> `` | The lists differ in length. |
| `` `find` is empty `` or `find <n> is empty` | A find with no text, which would match everywhere. |
| ``lacks `must_fail`: name the tests the mutation must fail`` | No `must_fail` key (section 7.1). |
| `` `must_fail` is empty: every entry must name at least one test that fails deterministically (a randomized test that only sometimes fails goes in `may_fail`) `` | An empty `must_fail` (section 7.1). |
| `` `may_fail` has no meaning with expect = "compile-error" `` | Section 7.2. |

### 9.3 Edits, refusals and the baseline

Edit problems are reported before any file is touched, all together, prefixed with the table and the entry's id.

| Message | Cause and fix |
|---|---|
| ``the code this entry targets has changed: <find> no longer occurs in <file>; re-examine the entry's claim against the current code, then update `find` `` | Section 5.2. |
| `<find> occurs <n> times in <file>; a find must occur exactly once, so add surrounding text to make it unique` | Section 5.1. When an entry has several finds, the find is shown as `find <k> (<text>)`, and a count taken on the edited text adds ` after the earlier finds were applied`. |
| `... (it contains CRLF; the file's line endings are LF)` | A find written with `\r\n` for a file with LF endings. Write the find with `\n`. |
| `<find> spans lines, and <file> mixes CRLF and LF line endings, so which ending it should match is ambiguous` | Section 5.3. Make the file's endings consistent, or use single-line finds. |
| `the edit changes nothing (each replace equals its find)` | Section 5.4. |

A refusal prints its reason and touches nothing. The two refusals print a list of files.

```text
error: an earlier run was stopped before it restored these files:
  C:\dev\mycrate\src\lib.rs (saved copy: C:\dev\mycrate\target\ostiarius\.originals\@mycrate-1a2b3c4d\src\lib.rs)
  (no destination recorded) saved copy: C:\dev\mycrate\target\ostiarius\.originals\@x-00000000\src\a.rs
Run `ostiarius mutate --restore` with the same --log-dir, check the files, then run again.
```

```text
error: these target files have uncommitted changes (`??` is untracked), and a mutation left behind by a stopped run would look the same:
   M src/lib.rs    (in C:\dev\mycrate)
Commit or stash them first, or run with --allow-dirty.
```

| Message | Cause and fix |
|---|---|
| `note: <root> is not in a git work tree (or git is not installed); the uncommitted-changes check was skipped` | Not an error. The other refusals still apply. |
| `the unmutated tree already fails (exit <code>, failing <tests>) (see <log>). Nothing was mutated.` | The baseline failed. Fix the tree first. |
| `the unmutated tree did not build or printed no test results (see <log>). Nothing was mutated.` | The baseline did not build, or the command is not a test run. |
| `only <n> tests passed on the unmutated tree; the table needs at least <m> (see <log>). Nothing was mutated.` | The baseline passed fewer tests than `min_passed`, usually because a filter matches less than it used to. |
| `the baseline did not finish within <time> (see <log>). Nothing was mutated.` | The baseline outlived its limit. |
| `cannot touch <path>: <reason>` or `could not run <command>: <reason>` | A file's time could not be set, or the command could not start. |

### 9.4 Entry verdicts

| Message | Cause |
|---|---|
| `FAIL <id> <what>: the command passed (<n> passed); nothing caught it (see <log>)` | No test failed, so the argument is not load-bearing for these tests, or the edit misses the code path the tests exercise. |
| `FAIL <id> <what>: missing [<tests>], unexpected [<tests>] (see <log>)` | Named tests did not fail, or other tests did. The log shows which tests failed. |
| `FAIL <id> <what>: the mutated tree did not build (see <log>)` | The edit broke the build. Use `expect = "compile-error"` if that is the claim. |
| `FAIL <id> <what>: expected a build error; it built (see <log>)` | An `expect = "compile-error"` entry built. |
| `FAIL <id> <what>: the command failed without test results, but not with a compiler error (see <log>)` | An `expect = "compile-error"` entry failed for another reason. |
| `ok   <id> <what>: rejected by the compiler` | Section 7.2. |
| `<line>; also <tests> (may fail)` | Section 7.1. |

## 10. Versions

The format is versioned so that a table is never misread. Any change to the meaning of an existing key, or any new required key, raises the version. A table declaring `version = 1` will always be read by version 1's rules or refused with a message naming the version, and never silently read under different ones.
