# skim — SQL dump to structured format converter

> Stream large SQL dump files (INSERT statements) to JSON, JSONL, CSV, YAML, or Parquet.
> Handles files of 1–20 GB without loading them into memory.

---

## Problem

Tools like `mysqldump` and `pg_dump` produce multi-gigabyte `.sql` files that
are hard to query or analyse. This CLI converts them to structured formats
usable by pandas, DuckDB, Excel, and similar tools.

---

## CLI

Format is inferred from the **output file extension**. Default is JSON.

```
skim [OPTIONS] [INPUT]

ARGS:
  [INPUT]    SQL dump file. Omit or '-' for stdin.

OPTIONS:
  -o, --output <FILE>   Output file (extension sets format). Omit for stdout.
  -f, --format <FMT>    Override format: json | jsonl | csv | yaml | parquet
  -t, --table  <NAME>   Filter to this table (repeatable).
      --infer-rows <N>  Rows buffered for Parquet type inference [default: 1000]
      --batch-size <N>  Rows per Arrow RecordBatch, Parquet only [default: 10000]
      --max-statement-size <B>  Circuit breaker [default: 268435456]
      --no-header       Suppress CSV header row
      --progress        Show progress bar on stderr
```

**Format resolution order:** `--format` flag → file extension → JSON (default)

| Extension | Format |
|-----------|--------|
| `.json`   | JSON array |
| `.jsonl`  | Newline-delimited JSON |
| `.csv`    | CSV |
| `.yaml` / `.yml` | YAML |
| `.toml`   | TOML (array of tables) |
| `.parquet` | Apache Parquet |
| *(anything else / stdout)* | **JSON** |

**Examples:**
```bash
skim dump.sql                          # JSON on stdout
skim -o users.csv dump.sql             # CSV (from .csv extension)
skim -o data.parquet dump.sql          # Parquet
skim -o out.jsonl -t users dump.sql    # JSONL, filter by table
gzip -dc dump.sql.gz | skim -f csv    # CSV on stdout via --format override
skim --progress -o out.csv dump.sql   # CSV with progress bar
```

---

## Architecture

Two-stage streaming pipeline — never loads more than one SQL statement:

```
File / stdin
  │  BufReader (64 KB I/O buffer)
  ▼
StatementExtractor   ← state-machine, yields one complete statement at a time
  ▼
StatementDispatcher
  ├── CREATE TABLE ──► SchemaExtractor → Schema
  └── INSERT INTO  ──► InsertParser   → Vec<Row>
                              ▼
                         Writer::write_row()
                              ├── JsonWriter   (json.rs)
                              ├── JsonlWriter  (jsonl.rs)
                              ├── CsvWriter    (csv.rs)
                              ├── YamlWriter   (yaml.rs)
                              └── ParquetWriter (parquet.rs)
```

### Why two stages?

`sqlparser` needs a complete `&str` — it cannot parse byte-by-byte.
The state machine extracts exactly one statement at a time; each statement
string is then handed to `sqlparser` for correct value parsing
(`\'`, `''`, hex literals, NULL, etc.).

---

## Module layout

```
src/
  main.rs                 entry point — wires args → pipeline
  cli.rs                  clap Args + OutputFormat + resolved_format()
  error.rs                ConvertError (thiserror)
  progress.rs             indicatif progress-bar wrapper
  parser/
    mod.rs                Value, Row, Column, InferredType, Schema
    state_machine.rs      StatementExtractor<R: BufRead>
    value_parser.rs       sqlparser AST → Vec<Row>
    schema.rs             CREATE TABLE AST → Schema
  writer/
    mod.rs                Writer trait
    json.rs               JSON array
    jsonl.rs              newline-delimited JSON
    csv.rs                CSV
    yaml.rs               YAML
    parquet.rs            Apache Parquet (two-phase inference)
```

---

## Key data types

```rust
pub enum Value { Null, Bool(bool), Integer(i64), Float(f64), Text(String), Bytes(Vec<u8>) }
pub struct Row    { pub values: Vec<Value> }
pub struct Column { pub name: String, pub inferred_type: InferredType }
pub enum InferredType { Unknown, Boolean, Int64, Float64, Utf8 }
pub struct Schema { pub table_name: String, pub columns: Vec<Column> }
```

---

## Parquet schema inference

**Preferred:** If a `CREATE TABLE` statement appears before any `INSERT`, extract
column names and map SQL types to Arrow types exactly.

**Fallback (no CREATE TABLE):** Buffer the first `--infer-rows` rows. Walk
column-by-column to find the widest compatible type:
`Unknown → Boolean → Int64 → Float64 → Utf8`. After inference, flush buffer
as the first RecordBatch, then continue streaming.

Batch flushing: every `--batch-size` rows (default 10 000) via Arrow builders
→ `ArrowWriter::write(RecordBatch)`.

---

## Implementation phases

| # | Phase | Status |
|---|-------|--------|
| 1 | Scaffold — Cargo.toml, cli.rs, empty stubs | ✅ done |
| 2 | Core data types — Value, Row, Schema, Error | ✅ done |
| 3 | State machine — StatementExtractor + 12 tests | ✅ done |
| 4 | SQL value parser — sqlparser AST → Row / Schema | ✅ done |
| 5 | Writers — JSONL, JSON, CSV, YAML, TOML, Parquet | ✅ done |
| 6 | Main pipeline — wire everything together | ✅ done |
| 7 | Polish — real dump files, memory/perf tests | ✅ done |

---

## Dependency reference

| Crate | Purpose | Docs |
|-------|---------|------|
| `clap 4` | CLI parsing | [docs.rs/clap](https://docs.rs/clap/latest/clap/) |
| `anyhow` | Error propagation | [docs.rs/anyhow](https://docs.rs/anyhow/latest/anyhow/) |
| `thiserror` | Custom error types | [docs.rs/thiserror](https://docs.rs/thiserror/latest/thiserror/) |
| `sqlparser 0.56` | SQL AST | [docs.rs/sqlparser](https://docs.rs/sqlparser/latest/sqlparser/) |
| `serde` / `serde_json` | JSON | [serde.rs](https://serde.rs/) |
| `csv` | CSV writing | [docs.rs/csv](https://docs.rs/csv/latest/csv/) |
| `serde_yaml` | YAML | [docs.rs/serde_yaml](https://docs.rs/serde_yaml/latest/serde_yaml/) |
| `parquet 54` | Parquet | [docs.rs/parquet](https://docs.rs/parquet/latest/parquet/) |
| `arrow-array 54` | Arrow builders | [docs.rs/arrow-array](https://docs.rs/arrow-array/latest/arrow_array/) |
| `arrow-schema 54` | Arrow schema/types | [docs.rs/arrow-schema](https://docs.rs/arrow-schema/latest/arrow_schema/) |
| `indicatif` | Progress bar | [docs.rs/indicatif](https://docs.rs/indicatif/latest/indicatif/) |
| `memchr` | Fast byte scan | [docs.rs/memchr](https://docs.rs/memchr/latest/memchr/) |

---

## Verification

```bash
# Help
skim --help

# Compile-time check
cargo check

# Unit tests
cargo test

# JSONL on stdout
skim dump.sql

# CSV with table filter
skim -t users -o users.csv dump.sql

# Parquet
skim -o data.parquet dump.sql
python3 -c "import pandas as pd; print(pd.read_parquet('data.parquet').head())"

# Memory check — should stay well under 500 MB for a 5 GB input
/usr/bin/time -v skim -o out.jsonl big.sql
```
