//! Integration tests that verify every meaningful CLI invocation shown in the README.
//!
//! Each test:
//!   1. Writes a small SQL fixture to a temp directory.
//!   2. Runs the `skim` binary via `std::process::Command`.
//!   3. Asserts on exit code, stdout, or output files.
//!
//! Run with:
//!   cargo test --test readme_examples

use std::fs;
use std::io::Write;
use std::process::{Command, Stdio};

const README: &str = include_str!("../README.md");
const AGENTS: &str = include_str!("../AGENTS.md");

// Cargo sets this env var to the path of the compiled binary for integration tests.
fn skim() -> Command {
    Command::new(env!("CARGO_BIN_EXE_skim"))
}

// ── Fixtures ──────────────────────────────────────────────────────────────────

/// Small MySQL-style dump with `users` (3 rows) and `orders` (3 rows).
/// Covers: INT, VARCHAR, DECIMAL, TINYINT(1), NULL, multi-row VALUES.
const MYSQL_FIXTURE: &str = r#"
-- MySQL dump
CREATE TABLE `users` (
  `id` INT NOT NULL,
  `name` VARCHAR(100) DEFAULT NULL,
  `email` VARCHAR(100) DEFAULT NULL,
  `active` TINYINT(1) DEFAULT NULL
);

INSERT INTO `users` (`id`, `name`, `email`, `active`) VALUES
  (1,'Alice','alice@example.com',1),
  (2,'Bob','bob@example.com',0),
  (3,'Carol','carol@example.com',1);

CREATE TABLE `orders` (
  `id` INT NOT NULL,
  `user_id` INT DEFAULT NULL,
  `total` DECIMAL(10,2) DEFAULT NULL
);

INSERT INTO `orders` (`id`, `user_id`, `total`) VALUES
  (1,1,99.99),
  (2,1,49.50),
  (3,2,149.00);
"#;

/// Small PostgreSQL pg_dump --inserts style fixture.
/// Covers: schema-qualified names, double-quoted identifiers, :: casts.
const PG_FIXTURE: &str = r#"
-- PostgreSQL database dump

CREATE TABLE public.events (
  id INTEGER,
  name TEXT,
  score NUMERIC
);

INSERT INTO public.events (id, name, score) VALUES
  (1, 'Alpha', 9.5::numeric),
  (2, 'Beta', 7.0::numeric);
"#;

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Write `contents` to `<dir>/<filename>` and return the full path as a String.
fn write_fixture(dir: &tempfile::TempDir, filename: &str, contents: &str) -> String {
    let path = dir.path().join(filename);
    fs::write(&path, contents).expect("failed to write fixture");
    path.to_str().unwrap().to_owned()
}

/// Assert the command succeeded (exit code 0), returning stdout as a String.
fn assert_success(cmd: &mut Command) -> String {
    let out = cmd.output().expect("failed to run skim");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        out.status.success(),
        "skim exited with {}\nstdout: {stdout}\nstderr: {stderr}",
        out.status,
    );
    stdout
}

// ── README example tests ──────────────────────────────────────────────────────

#[test]
fn readme_documents_supported_cli_flags() {
    let help = assert_success(skim().arg("--help"));

    let documented_flags = [
        "--output",
        "--format",
        "--table",
        "--no-header",
        "--no-progress",
        "--dialect",
        "--infer-rows",
        "--batch-size",
        "--max-statement-size",
    ];

    for flag in documented_flags {
        assert!(
            README.contains(flag),
            "README should document CLI flag {flag}",
        );
        assert!(help.contains(flag), "CLI help should expose {flag}");
    }

    assert!(
        !README.contains("--progress"),
        "README must not document unsupported --progress flag; use --no-progress",
    );
}

#[test]
fn agents_file_documents_test_author_implementer_workflow() {
    for required in [
        "Test Author",
        "Implementer",
        "Reviewer",
        "tests/readme_examples.rs",
        "cargo test",
        "cargo check --release",
    ] {
        assert!(
            AGENTS.contains(required),
            "AGENTS.md should include guidance for {required}",
        );
    }
}

/// README: `skim dump.sql`
/// Default output is a JSON object grouped by table name.
#[test]
fn default_output_is_json() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = write_fixture(&dir, "dump.sql", MYSQL_FIXTURE);

    let stdout = assert_success(skim().arg(&fixture));

    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .expect("stdout should be valid JSON");
    assert!(parsed.is_object(), "expected a JSON object grouped by table");
    // Both tables present as keys
    assert_eq!(parsed["users"].as_array().unwrap().len(), 3, "expected 3 users rows");
    assert_eq!(parsed["orders"].as_array().unwrap().len(), 3, "expected 3 orders rows");
}

/// README: `skim --format jsonl dump.sql`
/// Each line of stdout is a valid JSON object.
#[test]
fn format_flag_jsonl() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = write_fixture(&dir, "dump.sql", MYSQL_FIXTURE);

    let stdout = assert_success(skim().args(["--format", "jsonl", &fixture]));

    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 6, "expected 6 JSONL lines (3 users + 3 orders)");
    for line in &lines {
        let v: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|_| panic!("invalid JSON on line: {line}"));
        assert!(v.is_object(), "each JSONL line should be an object");
    }
}

/// README: `cat dump.sql | skim --format jsonl -`
/// Stdin pipe produces the same output as reading from a file.
#[test]
fn stdin_pipe_jsonl() {
    let mut child = skim()
        .args(["--format", "jsonl", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn skim");

    child.stdin.take().unwrap().write_all(MYSQL_FIXTURE.as_bytes()).unwrap();

    let out = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        out.status.success(),
        "skim stdin pipe failed\nstdout: {stdout}\nstderr: {stderr}",
    );

    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 6, "stdin pipe should produce 6 JSONL lines");
}

#[cfg(debug_assertions)]
#[test]
fn debug_env_prints_performance_summary() {
    let mut child = skim()
        .args(["--no-progress", "-"])
        .env("SKIM_DEBUG", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn skim");

    child.stdin.take().unwrap().write_all(MYSQL_FIXTURE.as_bytes()).unwrap();

    let out = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "skim failed\nstderr: {stderr}");
    assert!(stderr.contains("[skim debug] enabled by SKIM_DEBUG=1"));
    assert!(stderr.contains("[skim debug] parsing insert statement="));
    assert!(stderr.contains("row_parse="));
    assert!(stderr.contains("rows/s="));
}

#[test]
fn debug_summary_hidden_without_env() {
    let mut child = skim()
        .args(["--no-progress", "-"])
        .env_remove("SKIM_DEBUG")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn skim");

    child.stdin.take().unwrap().write_all(MYSQL_FIXTURE.as_bytes()).unwrap();

    let out = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "skim failed\nstderr: {stderr}");
    assert!(
        !stderr.contains("[skim debug]"),
        "debug output should be hidden without SKIM_DEBUG, got: {stderr}",
    );
}

#[test]
fn mysql_versioned_disable_enable_keys_comments_are_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = write_fixture(
        &dir,
        "dump.sql",
        r#"
CREATE TABLE `HadithTable` (
  `id` INT NOT NULL,
  `text` TEXT
);
LOCK TABLES `HadithTable` WRITE;
/*!40000 ALTER TABLE `HadithTable` DISABLE KEYS */;
INSERT INTO `HadithTable` VALUES
  (1,'first'),
  (2,'second');
/*!40000 ALTER TABLE `HadithTable` ENABLE KEYS */;
UNLOCK TABLES;
"#,
    );

    let stdout = assert_success(skim().args(["--no-progress", &fixture]));

    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .expect("stdout should be valid JSON");
    let rows = parsed["HadithTable"].as_array().expect("HadithTable rows");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["text"], "first");
}

/// README: `skim -t users dump.sql -o users.csv`
/// CSV output for a single table: header row + exactly 3 data rows.
#[test]
fn table_filter_csv() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = write_fixture(&dir, "dump.sql", MYSQL_FIXTURE);
    let out_path = dir.path().join("users.csv");

    assert_success(
        skim().args(["-t", "users", &fixture, "-o", out_path.to_str().unwrap()]),
    );

    let content = fs::read_to_string(&out_path).expect("users.csv not created");
    let lines: Vec<&str> = content.lines().collect();
    // Header + 3 data rows
    assert_eq!(lines.len(), 4, "expected header + 3 data rows, got: {lines:?}");
    // Header must contain the column names (not data values)
    assert!(
        lines[0].contains("id") && lines[0].contains("name"),
        "first line should be the CSV header, got: {}",
        lines[0],
    );
    // Data rows must contain commas and must not be the header
    for i in 1..=3 {
        assert!(
            lines[i].contains(','),
            "line {i} should be a CSV data row, got: {}",
            lines[i],
        );
    }
}

/// README: `skim -t users -t orders dump.sql`
/// Multiple table filters: JSON object has both table keys.
#[test]
fn multi_table_filter_json() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = write_fixture(&dir, "dump.sql", MYSQL_FIXTURE);

    let stdout = assert_success(skim().args(["-t", "users", "-t", "orders", &fixture]));

    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert!(parsed.is_object(), "expected grouped JSON object");
    let users = parsed["users"].as_array().unwrap();
    let orders = parsed["orders"].as_array().unwrap();
    assert_eq!(users.len() + orders.len(), 6, "expected 3 users + 3 orders = 6 rows");
}

/// README: `skim --no-progress dump.sql -o output.parquet`
/// Parquet file is created and non-empty.
#[test]
fn parquet_output() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = write_fixture(&dir, "dump.sql", MYSQL_FIXTURE);
    let out_path = dir.path().join("output.parquet");

    assert_success(skim().args(["--no-progress", &fixture, "-o", out_path.to_str().unwrap()]));

    let metadata = fs::metadata(&out_path).expect("output.parquet not created");
    assert!(metadata.len() > 0, "parquet file should not be empty");
}

/// Progress bar is on by default; `--no-progress` disables it. Exit code 0 either way.
/// Uses -t users to keep a consistent schema (CSV requires uniform column count).
#[test]
fn progress_flag_exits_ok() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = write_fixture(&dir, "dump.sql", MYSQL_FIXTURE);
    let out_path = dir.path().join("output.csv");

    assert_success(
        skim().args(["--no-progress", "-t", "users", &fixture, "-o", out_path.to_str().unwrap()]),
    );

    assert!(out_path.exists(), "output.csv should be created with --no-progress");
}

/// README: `skim --format csv --no-header dump.sql`
/// First line of output is a data row, not a column-name header.
/// Uses -t users to keep a consistent schema (CSV requires all rows to have the same column count).
#[test]
fn no_header_csv() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = write_fixture(&dir, "dump.sql", MYSQL_FIXTURE);

    let stdout = assert_success(
        skim().args(["--format", "csv", "--no-header", "-t", "users", &fixture]),
    );

    let first_line = stdout.lines().next().expect("stdout should not be empty");
    // Without a header the first line is the first data row.
    // It must contain commas (it's CSV data) and must NOT be the column-name header.
    assert!(
        first_line.contains(','),
        "first line should be a CSV data row, got: {first_line}",
    );
    assert!(
        !first_line.contains("name") && !first_line.contains("email"),
        "first line should be data, not a header, got: {first_line}",
    );
}

/// README: `skim --format yaml dump.sql -o out.yaml`
/// YAML output contains the `---` document separator.
#[test]
fn yaml_output() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = write_fixture(&dir, "dump.sql", MYSQL_FIXTURE);
    let out_path = dir.path().join("out.yaml");

    assert_success(
        skim().args(["--format", "yaml", &fixture, "-o", out_path.to_str().unwrap()]),
    );

    let content = fs::read_to_string(&out_path).expect("out.yaml not created");
    assert!(
        content.contains("---"),
        "YAML output should contain document separator '---'",
    );
}

/// README: `skim --format toml dump.sql -o out.toml`
/// TOML file is created and non-empty.
#[test]
fn toml_output() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = write_fixture(&dir, "dump.sql", MYSQL_FIXTURE);
    let out_path = dir.path().join("out.toml");

    assert_success(
        skim().args(["--format", "toml", &fixture, "-o", out_path.to_str().unwrap()]),
    );

    let content = fs::read_to_string(&out_path).expect("out.toml not created");
    assert!(!content.is_empty(), "TOML file should not be empty");
}

/// Format auto-detected from `.csv` extension (no --format flag needed).
/// Uses -t users to keep a consistent schema (CSV requires uniform column count).
#[test]
fn extension_detects_csv() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = write_fixture(&dir, "dump.sql", MYSQL_FIXTURE);
    let out_path = dir.path().join("out.csv");

    assert_success(
        skim().args(["-t", "users", &fixture, "-o", out_path.to_str().unwrap()]),
    );

    let content = fs::read_to_string(&out_path).expect("out.csv not created");
    // A CSV file has comma-separated values; header must be present
    assert!(
        content.lines().next().unwrap_or("").contains(','),
        "expected CSV header with commas",
    );
}

/// Format auto-detected from `.jsonl` extension (no --format flag needed).
#[test]
fn extension_detects_jsonl() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = write_fixture(&dir, "dump.sql", MYSQL_FIXTURE);
    let out_path = dir.path().join("out.jsonl");

    assert_success(skim().args([&fixture, "-o", out_path.to_str().unwrap()]));

    let content = fs::read_to_string(&out_path).expect("out.jsonl not created");
    assert_eq!(
        content.lines().count(),
        6,
        "JSONL file should have one line per row",
    );
}

/// README: `skim --dialect postgres pg_dump.sql`
/// PostgreSQL dump with schema-qualified names and `::` casts parses correctly.
#[test]
fn dialect_postgres() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = write_fixture(&dir, "pg_dump.sql", PG_FIXTURE);

    let stdout = assert_success(skim().args(["--dialect", "postgres", &fixture]));

    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert!(parsed.is_object(), "expected grouped JSON object");
    let rows = parsed["events"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "expected 2 rows from PG fixture");

    // Verify the schema-qualified table name was handled (values are correct)
    assert_eq!(rows[0]["id"], 1);
    assert_eq!(rows[0]["name"], "Alpha");
    assert_eq!(rows[1]["name"], "Beta");
}

/// Only rows matching `-t` filter are included; other tables are excluded.
#[test]
fn table_filter_excludes_other_tables() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = write_fixture(&dir, "dump.sql", MYSQL_FIXTURE);

    let stdout = assert_success(skim().args(["-t", "users", &fixture]));

    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert!(parsed.is_object(), "expected grouped JSON object");
    let rows = parsed["users"].as_array().unwrap();
    assert_eq!(rows.len(), 3, "only users (3 rows) should be included");
    // Every row should have the 'email' field (users column), not 'total' (orders column)
    for row in rows {
        assert!(row.get("email").is_some(), "row should have 'email' field");
        assert!(row.get("total").is_none(), "orders rows should be excluded");
    }
}
