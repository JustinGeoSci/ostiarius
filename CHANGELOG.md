# Changelog

All notable changes to this crate are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the crate follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.1] - 2026-11-20

### Changed

- A command that uses `-q` or `--quiet` is now refused, by `count` and in
  mutation tables, as `--nocapture` and `--show-output` already were. Quiet
  output leaves out Cargo's `Running` headers and libtest's per-test lines,
  so in 0.1.0 no binary could be labeled and no failing test named, and a
  mutation that worked was reported as a failure. `count` exits with code 3,
  and a table that uses either flag is refused at load before any file is
  touched.

### Fixed

- Doctests of an edition 2024 crate are read correctly. Since Rust 1.86,
  rustdoc merges these doctests into one binary and runs the ones it cannot
  merge, such as `compile_fail` tests, as further groups, each with its own
  result line under the one `Doc-tests` header. Version 0.1.0 counted more
  result lines than headers and reported the log as untrustworthy, so a run
  that passed exited with code 2. The groups are now added into one doctest
  binary.
- Doctest names that carry a mode after the line number, `- compile fail`,
  `- should panic` or `- compile`, are now normalized to the name a listing
  gives. In 0.1.0 such a test could not be matched by `--expect-names` or
  named in `must_fail`.

A doctest group is accepted only if its result line follows a `running N
tests` line of its own whose N equals the counts in that result. A stray
result line is still reported, inside the doctest section or after any other
binary.

## [0.1.0] - 2026-10-25

The first release has two subcommands.

`ostiarius count` asserts what a `cargo test` run passed. It reads saved logs in UTF-8 or UTF-16, or runs the command itself on one merged pipe so Cargo's headers stay in order with the results. It checks an exact or minimum total, a minimum for any single test binary, and that every test named in a file exists, optionally by listing tests without running them. A run in which no test passed fails unless `--allow-empty` is given. A test's captured output is never read as results, and a log with more result lines than test binaries is reported as untrustworthy. `--json` prints the result as one object.

`ostiarius mutate` runs mutation tables in format version 1, documented in `docs/table-format.md`. Each entry's edit is checked before any file is touched, its finds apply in order and must each occur exactly once, and multi-line finds match CRLF files. A run refuses saved copies left by a stopped run and uncommitted target files, and it runs a baseline on the unmutated tree that must pass and that sets the default timeout. Every mutated file is restored byte for byte and confirmed, including after a timeout, which kills the command's whole process tree, and after Ctrl-C, which exits with code 130. `--restore` recovers from a run that was killed outright.

The table format is versioned. Any change to the meaning of an existing key, or any new required key, will raise the version.

[0.1.1]: https://github.com/JustinGeoSci/ostiarius/releases/tag/v0.1.1
[0.1.0]: https://github.com/JustinGeoSci/ostiarius/releases/tag/v0.1.0