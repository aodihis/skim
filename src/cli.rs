use clap::{Parser, ValueEnum};
use std::path::PathBuf;

/// Convert SQL dump files (INSERT statements) to JSON, JSONL, CSV, YAML, TOML, or Parquet.
///
/// The output format is inferred from the output file extension.
/// Use --format to override or to set a format when writing to stdout.
///
/// Supported extensions:
///   .json    → JSON array
///   .jsonl   → Newline-delimited JSON (one object per line)
///   .csv     → CSV
///   .yaml / .yml → YAML
///   .toml    → TOML (array of tables)
///   .parquet → Parquet (requires a real file path, not stdout)
///
/// Default (no -o / unrecognised extension / stdout): JSON
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct Args {
    /// Input SQL dump file. Omit or use '-' to read from stdin.
    #[arg(value_name = "INPUT")]
    pub input: Option<PathBuf>,

    /// Output file path. Extension determines the format automatically.
    /// Omit or use '-' for stdout (defaults to JSON).
    /// Parquet requires a real file path.
    #[arg(short, long, value_name = "FILE")]
    pub output: Option<PathBuf>,

    /// Override the output format regardless of file extension.
    /// Useful when piping to stdout in a non-JSON format.
    #[arg(short, long, value_name = "FORMAT")]
    pub format: Option<OutputFormat>,

    /// Only convert rows from this table. Repeatable (e.g. -t users -t orders).
    #[arg(short, long = "table", value_name = "TABLE")]
    pub tables: Vec<String>,

    /// Number of rows to buffer for Parquet schema inference (no CREATE TABLE present).
    #[arg(long, default_value = "1000", value_name = "N")]
    pub infer_rows: usize,

    /// Number of rows per Arrow RecordBatch for Parquet output.
    #[arg(long, default_value = "10000", value_name = "N")]
    pub batch_size: usize,

    /// Abort if a single SQL statement exceeds this size in bytes.
    #[arg(long, default_value = "268435456", value_name = "BYTES")]
    pub max_statement_size: usize,

    /// Suppress the CSV header row.
    #[arg(long)]
    pub no_header: bool,

    /// Disable the progress bar (shown on stderr by default).
    #[arg(long)]
    pub no_progress: bool,

    /// SQL dialect of the dump file: mysql, postgres, or auto.
    /// 'auto' detects the dialect from the dump header comment (default).
    #[arg(long, default_value = "auto", value_name = "DIALECT")]
    pub dialect: CliDialect,
}

/// SQL dialect selection for the --dialect flag.
#[derive(ValueEnum, Clone, Debug, PartialEq)]
pub enum CliDialect {
    /// Detect from the dump header (-- MySQL dump / -- PostgreSQL database dump).
    Auto,
    /// MySQL / MariaDB dumps (backtick identifiers, \\' escapes).
    Mysql,
    /// PostgreSQL dumps (double-quote identifiers, $$ quoting, schema-qualified names).
    Postgres,
}

/// All supported output formats.
#[derive(ValueEnum, Clone, Debug, PartialEq)]
pub enum OutputFormat {
    /// JSON array  (e.g. output.json)
    Json,
    /// Newline-delimited JSON, one object per line  (e.g. output.jsonl)
    Jsonl,
    /// Comma-separated values  (e.g. output.csv)
    Csv,
    /// YAML documents  (e.g. output.yaml)
    Yaml,
    /// TOML array of tables  (e.g. output.toml)
    Toml,
    /// Apache Parquet  (e.g. output.parquet) — requires a real file path
    Parquet,
}

impl Args {
    /// Resolve the effective output format.
    ///
    /// Resolution order:
    /// 1. --format flag (explicit override)
    /// 2. Output file extension
    /// 3. Default → JSON
    pub fn resolved_format(&self) -> OutputFormat {
        // 1. Explicit flag wins.
        if let Some(f) = &self.format {
            return f.clone();
        }

        // 2. Infer from output file extension.
        if let Some(path) = &self.output {
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                return match ext {
                    e if e.eq_ignore_ascii_case("json")    => OutputFormat::Json,
                    e if e.eq_ignore_ascii_case("jsonl")   => OutputFormat::Jsonl,
                    e if e.eq_ignore_ascii_case("csv")     => OutputFormat::Csv,
                    e if e.eq_ignore_ascii_case("yaml")
                      || e.eq_ignore_ascii_case("yml")     => OutputFormat::Yaml,
                    e if e.eq_ignore_ascii_case("toml")    => OutputFormat::Toml,
                    e if e.eq_ignore_ascii_case("parquet") => OutputFormat::Parquet,
                    _                                      => OutputFormat::Json,
                };
            }
        }

        // 3. Default.
        OutputFormat::Json
    }

    /// True when the input is stdin (no path given, or path is "-").
    pub fn is_stdin(&self) -> bool {
        match &self.input {
            None => true,
            Some(p) => p.to_str() == Some("-"),
        }
    }

    /// True when the output is stdout (no path given, or path is "-").
    pub fn is_stdout(&self) -> bool {
        match &self.output {
            None => true,
            Some(p) => p.to_str() == Some("-"),
        }
    }
}
