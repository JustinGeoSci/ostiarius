# ostiarius

*The gatekeeper*

`ostiarius` checks whether a `cargo test` gate proves what it claims.

A test gate makes two kinds of claim that its exit code cannot support on its own. The first is a claim about coverage, that a filtered run exercised the tests it was meant to. `cargo test` exits successfully when a filter matches nothing, so a gate that names a module path which no longer exists passes silently. The second is a claim about an argument, that a particular comparison, bound or invariant in the code is what keeps the results right. A passing suite says nothing about which arguments its tests actually depend on. `ostiarius count` makes the first claim checkable by asserting how many tests passed, per test binary and by name. `ostiarius mutate` makes the second checkable by breaking each argument on purpose and requiring the tests that depend on it to fail.

## Installing

```text
cargo install --locked ostiarius
```

`--locked` builds with the exact dependency versions the release was tested with, recorded in its `Cargo.lock`, rather than the newest compatible ones. 

## Minimum supported Rust version

Rust 1.87.0, edition 2024.

## Counting tests

`ostiarius count` reads saved logs or runs the command itself, then checks the totals the user asked for. A run in which no test passed is a failure unless `--allow-empty` says that is expected.

```text
$ ostiarius count --min-passed 40 -- cargo test -p mycrate --release
...
42 passed, 0 failed, 1 ignored, in 3 test binaries
```

A total can hide a lost module, since a binary that lost twelve tests and another that gained twelve leave the sum unchanged. `--min-passed-in` sets a floor for one test binary, named by the label `--per-binary` prints. `--expect-names` requires that every test listed in a file exists, and with `--list-mode` it lists the tests without running them, which catches a renamed or deleted test that no count would reveal.

```text
$ ostiarius count --per-binary --min-passed-in "tests/parity.rs=12" -- cargo test -p mycrate
45 passed, 0 failed, 0 ignored, in 3 test binaries
  unittests src/lib.rs (mycrate): 30 passed, 0 failed, 0 ignored
  tests/parity.rs (parity): 13 passed, 0 failed, 0 ignored
  doctests (mycrate): 2 passed, 0 failed, 0 ignored

$ ostiarius count --expect-names names.txt --list-mode -- cargo test -p mycrate
45 tests listed
FAILED: tests not found: parity::renamed_case
```

A failing test can print anything, including a line that looks like a result, so a test's captured output is never read as results. For the same reason `count` refuses to run a command with `--nocapture` or `--show-output`. `--json` prints the result as one JSON object for CI.

## Testing an argument with a mutation table

A mutation table is a TOML file. Each entry names one edit that breaks an argument, and the tests that must fail when it is broken. The table below tests the closed comparison in `examples/intervals`, a function that decides whether two intervals share a point.

```toml
version = 1
root = "intervals"
command = ["cargo", "test", "--lib"]
min_passed = 5

[[mutation]]
id = "open-interval"
what = "the closed comparison made open"
file = "src/interval.rs"
find = "a.lo <= b.hi && b.lo <= a.hi"
replace = "a.lo < b.hi && b.lo < a.hi"
must_fail = ["interval::tests::touching_intervals_overlap"]
```

```text
$ ostiarius mutate examples/intervals.mutations.toml
root: /home/me/ostiarius/examples/intervals
ok   baseline: 5 passed in 0.5 s (entry timeout up to 60 s): cargo test --lib
ok   open-interval            the closed comparison made open: fails touching_intervals_overlap
ok   one-sided                one of the two bound checks dropped: fails adjacent_intervals_do_not, disjoint_intervals_do_not, overlap_is_symmetric
ok   off-by-one               the upper bound widened by one: fails adjacent_intervals_do_not, overlap_is_symmetric
ok   restored: 5 passed (need 5): cargo test --lib
```

An entry passes only if the mutated tree built, every test in `must_fail` failed, and any other failing test is listed in `may_fail`, which exists for randomized tests that catch a mutation on some runs and not on others. A mutation that no test notices is reported as `nothing caught it`, which means either that the argument is not load-bearing for these tests or that the edit misses the code path they exercise. `docs/table-format.md` is the complete reference for the table format, covering every key, rule and message.

## What a run does to your files

A run edits source files in your working tree, so it is built to put every byte back. Before a file is mutated its original is saved under `target/ostiarius/.originals/`, together with a manifest naming where each copy belongs. After the entry's command ends, the file is restored, its bytes are read back and compared with the original, and its modification time is set to now so Cargo rebuilds it rather than keeping a build of the mutated text. This happens whatever ended the command, whether the tests failed, the command outlived its timeout or Ctrl-C was pressed. A command that outlives its timeout has its whole process tree killed, including any test binary still running, and Ctrl-C restores the file before the run exits with code 130.

Only a run killed outright, by closing its window or ending its process, cannot restore the file it was mutating. Its saved copy stays behind, and the next run refuses to start until `ostiarius mutate --restore` has put the copies back. A run also refuses a target file with uncommitted changes in a git work tree, since a leftover mutation looks exactly like an uncommitted change, and `--allow-dirty` overrides that check. Before any file is touched, each distinct command runs once on the unmutated tree and must pass. A tree that already fails would otherwise make every entry fail with the same unexpected tests, which hides the real cause. That baseline also sets the default timeout, five times its own time and never less than 60 seconds.

## Tables that stop matching

When the code an entry targets changes, its `find` no longer occurs and the run stops before touching anything.

```text
error: tables/core.toml: overlap-open: the code this entry targets has changed: "a.lo <= b.hi && b.lo <= a.hi" no longer occurs in src/interval.rs; re-examine the entry's claim against the current code, then update `find`
```

This is the table doing its job rather than getting in the way. The entry recorded an argument about specific code, and that code has moved, so the claim the entry tested may no longer hold. `--dry-run` checks every edit in a table in seconds without building anything, which makes it a cheap check to run after a refactor.

## Windows and PowerShell

`ostiarius` runs on Windows, Linux and macOS. On Windows a few details differ. Logs saved by Windows PowerShell with `>` are UTF-16 and are read correctly, but PowerShell's own `2>&1` rewrites every line Cargo prints on stderr, which destroys the `Running` headers that per-binary labels depend on. Merge the streams in `cmd` instead, as in `cmd /c "cargo test 2>&1" > test.log`, or let `count` run the command itself. A table's command is best written as a list of arguments, because a command string is split by POSIX quoting rules that remove the backslashes from an unquoted Windows path. Hung test processes are killed with `taskkill /T`.

## Exit codes

| Code | `count` | `mutate` |
|---|---|---|
| 0 | Every check passed. | Every entry and every restored run passed. |
| 1 | A test failed, the command failed, or a count or name check failed. | An entry or a restored run failed or timed out. |
| 2 | No result could be read, or the log cannot be trusted. | The run was refused, a table or an edit has a problem, or a file could not be restored. |
| 3 | A usage error. | A usage error. |
| 130 | | Ctrl-C stopped the run after restoring the file. |

## Compared with other tools

`cargo-nextest` already treats a run in which no test was selected as a failure, and has done so by default since 0.9.85. It does not assert a count, a per-binary floor or that named tests exist, and it does not read `cargo test` output, which is the gap `count` fills. `cargo-mutants` generates mutants automatically across a codebase and asks whether the tests catch arbitrary changes, with no changes to the source required. `ostiarius mutate` answers a narrower question. It tests a specific, declared argument and requires named tests to fail, which shows that those tests depend on that argument rather than that the suite catches changes in general. The two are complementary, and a project can use both.

## Limits

`ostiarius` reads libtest's human-readable output only, so nextest and other runners are not supported yet. Each mutation edits one file. Tests are matched by their full path, so two integration-test binaries containing a test with the same path cannot be told apart in `must_fail`.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option. See also [NOTICE](NOTICE).

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.

