# Guidance for Project Agents

## Project Overview

Skim is a Rust CLI that streams SQL dump files and converts `INSERT` rows to
JSON, JSONL, CSV, YAML, TOML, or Parquet without loading the whole dump into
memory.

Architecture:

- `src/main.rs` - CLI orchestration, dialect detection, statement routing, and
  writer dispatch.
- `src/cli.rs` - Clap argument definitions and output-format resolution.
- `src/parser/state_machine.rs` - Streaming SQL statement extraction. This is
  intentionally byte-oriented and protects semicolons inside strings/comments.
- `src/parser/schema.rs` - `CREATE TABLE` schema extraction through
  `sqlparser`.
- `src/parser/value_parser.rs` - `INSERT` row extraction through `sqlparser`.
- `src/writer/` - Output writers. JSON and JSONL are streaming writers; Parquet
  batches rows through Arrow.
- `src/debug_stats.rs` - Debug-build-only performance instrumentation, enabled
  at runtime with `SKIM_DEBUG`.
- `tests/readme_examples.rs` - Integration tests for documented CLI behavior and
  README examples.

The main performance-sensitive path is:

1. `StatementExtractor` yields one SQL statement.
2. `main.rs` skips unsupported non-data statements cheaply.
3. `schema.rs` handles `CREATE TABLE`.
4. `value_parser.rs` handles matching `INSERT` rows.
5. A `Writer` streams rows to the selected output format.

## Codebase Style and Guidelines

- Keep changes narrow and boring. This project is a converter; correctness,
  streaming behavior, and predictable CLI output matter more than clever
  abstractions.
- Preserve streaming behavior. Do not introduce whole-file reads for SQL input
  or output formats unless a format truly requires buffering.
- Prefer existing parser and writer boundaries over adding new cross-cutting
  helpers.
- Add helpers only when they remove real duplication or isolate a meaningful
  invariant. If a helper has one callsite and hides simple control flow, inline
  it.
- Comments should explain invariants, SQL edge cases, or performance tradeoffs.
  Avoid comments that merely restate the code.
- Unsupported SQL dump control statements should normally be skipped before
  reaching `sqlparser`; exporters should not fail on statements that do not
  affect row data.
- Keep generated/export artifacts out of commits. Large local files such as SQL
  dumps and converted JSON outputs are test inputs/outputs, not source.

## Testing Discipline

When changing behavior, think like two separate people:

- **Test Author:** first describe the externally correct behavior with a focused
  test. The test should be able to fail against the old behavior for the right
  reason.
- **Implementer:** then change the smallest production code needed to make that
  behavior pass.
- **Reviewer:** after implementation, reread the test as if someone else wrote
  it. Make sure it asserts the user-visible contract, not incidental details of
  your implementation.

For README and CLI work, tests must protect the documentation contract:

- If a README command is meaningful, add or update an integration test in
  `tests/readme_examples.rs`.
- If a CLI flag is documented, make sure the binary actually supports it.
- If a CLI behavior changes, update README and the corresponding test in the
  same change.

## Development Environment

This repository is a normal external Cargo checkout.

Useful commands:

- `cargo test` - run unit and integration tests.
- `cargo test --test readme_examples` - run CLI/README integration tests.
- `cargo check --release` - verify release builds. Debug-only instrumentation
  must compile out cleanly here.
- `cargo fmt --check` - check formatting. The repository currently has
  pre-existing formatting drift, so avoid broad formatting churn unless the user
  explicitly asks for it.

Performance debugging:

- Debug builds can print performance metrics when `SKIM_DEBUG` is set.
- Metrics are written to stderr so stdout/file output is never corrupted.
- Release builds must not print debug metrics and should keep instrumentation as
  no-op stubs.

## Source Control

Use Git in this repository.

- Check `git status --short --branch` before committing.
- Do not commit untracked SQL dumps, generated JSON/CSV/YAML/TOML/Parquet files,
  or unrelated local artifacts.
- Do not revert user changes unless the user explicitly asks.
- Commit messages should explain why the change exists and what user-visible
  behavior it protects. Avoid a laundry list of edited files.

## Before Handing Off

At minimum:

1. Run the most focused test for the behavior changed.
2. Run `cargo test` when the change affects parser, writer, or CLI behavior.
3. Run `cargo check --release` when touching debug-only code or feature gates.
4. Report any command that could not be run and why.
