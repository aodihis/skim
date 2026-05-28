mod cli;
mod error;
mod parser;
mod progress;
mod writer;

use std::{
    fs::File,
    io::{self, BufRead, BufReader, BufWriter, Write},
};

use clap::Parser;

use cli::{Args, CliDialect, OutputFormat};
use error::ConvertError;
use parser::{
    Schema, SqlDialect,
    schema::extract_schema,
    state_machine::StatementExtractor,
    value_parser::{extract_rows, insert_table_name},
};
use writer::{
    Writer,
    csv::CsvWriter,
    json::JsonWriter,
    jsonl::JsonlWriter,
    parquet::ParquetWriter,
    toml::TomlWriter,
    yaml::YamlWriter,
};

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let format = args.resolved_format();

    // Parquet cannot stream to stdout — it needs a seekable file.
    if format == OutputFormat::Parquet && args.is_stdout() {
        return Err(ConvertError::ParquetRequiresFile.into());
    }

    // ── Open input ────────────────────────────────────────────────────────────
    let file_len: Option<u64>;
    let mut raw_reader: Box<dyn BufRead> = if args.is_stdin() {
        file_len = None;
        Box::new(BufReader::new(io::stdin()))
    } else {
        let path = args.input.as_ref().unwrap();
        let f = File::open(path)?;
        file_len = f.metadata().ok().map(|m| m.len());
        Box::new(BufReader::new(f))
    };

    // ── Detect SQL dialect ────────────────────────────────────────────────────
    // For 'auto', peek at the first buffered chunk without consuming any bytes.
    // fill_buf() fills the BufReader's internal buffer and returns a reference
    // to it — nothing is consumed until consume() is called.
    let dialect = match args.dialect {
        CliDialect::Mysql    => SqlDialect::Mysql,
        CliDialect::Postgres => SqlDialect::Postgres,
        CliDialect::Auto     => {
            let buf = raw_reader.fill_buf()?;
            let peek = &buf[..buf.len().min(2048)];
            if memchr::memmem::find(peek, b"PostgreSQL database dump").is_some() {
                SqlDialect::Postgres
            } else {
                SqlDialect::Mysql
            }
        }
    };

    // ── Optional progress bar ─────────────────────────────────────────────────
    let bar;
    let reader: Box<dyn BufRead> = if args.progress {
        let b = progress::make_bar(file_len);
        let wrapped = Box::new(progress::ProgressReader::new(raw_reader, b.clone()));
        bar = Some(b);
        wrapped
    } else {
        bar = None;
        raw_reader
    };

    // ── Create writer ─────────────────────────────────────────────────────────
    let mut writer: Box<dyn Writer> = match format {
        OutputFormat::Parquet => {
            let path = args.output.as_ref().unwrap(); // stdout already rejected above
            Box::new(ParquetWriter::new(path, args.batch_size, args.infer_rows)?)
        }
        _ => {
            let out: Box<dyn Write> = if args.is_stdout() {
                Box::new(BufWriter::new(io::stdout()))
            } else {
                let path = args.output.as_ref().unwrap();
                Box::new(BufWriter::new(File::create(path)?))
            };
            match format {
                OutputFormat::Json    => Box::new(JsonWriter::new(out)),
                OutputFormat::Jsonl   => Box::new(JsonlWriter::new(out)),
                OutputFormat::Csv     => Box::new(CsvWriter::new(out, args.no_header)),
                OutputFormat::Yaml    => Box::new(YamlWriter::new(out)),
                OutputFormat::Toml    => Box::new(TomlWriter::new(out)),
                OutputFormat::Parquet => unreachable!(),
            }
        }
    };

    // ── Stream SQL ────────────────────────────────────────────────────────────
    let extractor = StatementExtractor::new(reader, args.max_statement_size);
    let mut schema = Schema { table_name: String::new(), columns: vec![] };
    let mut header_written = false;
    let mut row_count = 0u64;

    for stmt_result in extractor {
        let stmt = stmt_result?;

        // CREATE TABLE → adopt schema if the table passes the filter.
        if let Some(s) = extract_schema(&stmt, dialect)? {
            if table_matches(&s.table_name, &args.tables) {
                schema = s;
            }
            continue;
        }

        // Non-INSERT statements (SET, LOCK, UNLOCK, …) — skip cheaply.
        let trimmed = stmt.trim_start();
        if !trimmed.get(..7).map_or(false, |p| p.eq_ignore_ascii_case("INSERT ")) {
            continue;
        }

        // INSERT → check table name against filter.
        let tname = match insert_table_name(&stmt, dialect)? {
            Some(n) => n,
            None    => continue,
        };
        if !table_matches(&tname, &args.tables) {
            continue;
        }

        // First matching INSERT: write the header once.
        if !header_written {
            writer.write_header(&schema)?;
            header_written = true;
        }

        let rows = extract_rows(&stmt, &schema, dialect)?;
        for row in rows {
            row_count += 1;
            writer.write_row(&schema, &row)?;
        }
    }

    // Ensure write_header is always called (required by JSON/CSV/Parquet writers).
    if !header_written {
        writer.write_header(&schema)?;
    }
    writer.finish()?;

    if let Some(b) = bar {
        b.finish_with_message(format!("{row_count} row(s)"));
    }

    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// True when `name` matches any entry in `filter`.
/// Strips schema prefix (`public.users` → `users`) and wrapping quotes/backticks.
/// Always returns `true` when `filter` is empty (pass-all).
fn table_matches(name: &str, filter: &[String]) -> bool {
    if filter.is_empty() {
        return true;
    }
    // Take only the last dot-separated segment to drop schema prefixes.
    let unqualified = name.rsplit('.').next().unwrap_or(name);
    let clean = unqualified.trim_matches('`').trim_matches('"').to_lowercase();
    filter.iter().any(|t| t.to_lowercase() == clean)
}
