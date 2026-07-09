mod cli;
mod debug_stats;
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
use debug_stats::DebugStats;
use error::ConvertError;
use parser::{
    Schema, SqlDialect,
    schema::extract_schema,
    state_machine::StatementExtractor,
    value_parser::extract_insert_rows,
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
    let reader: Box<dyn BufRead> = if !args.no_progress {
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
    let mut debug = DebugStats::new(file_len);

    for stmt_result in extractor {
        let stmt = stmt_result?;
        debug.record_statement(stmt.len());

        // Skip past leading comments before deciding whether a statement is
        // worth parsing. MySQL versioned comments (`/*!40000 ... */`) can
        // contain dump-control SQL such as `ALTER TABLE ... DISABLE KEYS`
        // that sqlparser does not support and that exporters should ignore.
        let effective = skip_leading_comments(&stmt);
        if effective.is_empty() {
            debug.record_skipped_statement();
            continue;
        }

        // CREATE TABLE → adopt schema if the table passes the filter.
        if starts_with_keyword(effective, "CREATE") {
            debug.record_create_statement();
            let timer = debug.timer();
            let extracted_schema = extract_schema(effective, dialect)?;
            debug.add_schema_parse(timer.elapsed());
            if let Some(s) = extracted_schema {
                if table_matches(&s.table_name, &args.tables) {
                    schema = s;
                }
            }
            continue;
        }

        // Non-INSERT statements (SET, LOCK, UNLOCK, …) — skip cheaply.
        // Skip past any leading comments before checking the keyword, because
        // mysqldump often emits a comment block immediately before INSERT with
        // no semicolon in between, so the comment and INSERT end up in the same
        // extracted statement.
        if !starts_with_keyword(effective, "INSERT") {
            debug.record_skipped_statement();
            continue;
        }
        debug.record_insert_statement();
        debug.print_insert_parse_start(effective.len());

        let timer = debug.timer();
        let Some(insert) = extract_insert_rows(effective, &schema, dialect)? else {
            continue;
        };
        debug.add_row_parse(timer.elapsed());
        let Some(tname) = insert.table_name else {
            continue;
        };
        if !table_matches(&tname, &args.tables) {
            continue;
        }
        let rows = insert.rows;
        debug.record_rows(rows.len());

        // First matching INSERT: write the header once.
        if !header_written {
            writer.write_header(&schema)?;
            header_written = true;
        }

        let timer = debug.timer();
        for row in rows {
            row_count += 1;
            writer.write_row(&schema, &row)?;
            // Keep the spinner message fresh for stdin (no byte progress available).
            if let Some(b) = &bar {
                if row_count.is_multiple_of(500) {
                    b.set_message(format!("{row_count} rows"));
                }
            }
        }
        debug.add_row_write(timer.elapsed());
    }

    // Ensure write_header is always called (required by JSON/CSV/Parquet writers).
    if !header_written {
        writer.write_header(&schema)?;
    }
    let timer = debug.timer();
    writer.finish()?;
    debug.add_final_write(timer.elapsed());

    if let Some(b) = bar {
        b.finish_with_message(format!("{row_count} row(s)"));
    }
    debug.print_summary();

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

/// Return the first non-comment, non-whitespace portion of a SQL statement.
///
/// mysqldump places a comment block (`-- ...`) immediately before an INSERT
/// with no intervening semicolon, so both end up in the same extracted
/// statement.  This helper skips past those comments so we can check the
/// actual keyword.
fn skip_leading_comments(sql: &str) -> &str {
    let mut s = sql.trim_start();
    loop {
        if s.starts_with("--") {
            // Skip to end of line.
            s = match s.find('\n') {
                Some(pos) => s[pos + 1..].trim_start(),
                None      => return "",
            };
        } else if s.starts_with("/*") {
            // Skip block comment.
            s = match s.find("*/") {
                Some(pos) => s[pos + 2..].trim_start(),
                None      => return "",
            };
        } else {
            return s;
        }
    }
}

fn starts_with_keyword(sql: &str, keyword: &str) -> bool {
    let s = sql.trim_start();
    let Some(prefix) = s.get(..keyword.len()) else {
        return false;
    };
    if !prefix.eq_ignore_ascii_case(keyword) {
        return false;
    }
    s[keyword.len()..]
        .chars()
        .next()
        .is_none_or(|c| c.is_ascii_whitespace())
}
