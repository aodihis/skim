# skim

Stream a MySQL/MariaDB SQL dump and convert `INSERT` rows to JSON, JSONL, CSV, YAML, TOML, or Parquet — without loading the whole file into memory.

## Features

- Streaming parser — handles arbitrarily large dump files
- Six output formats: JSON, JSONL, CSV, YAML, TOML, Parquet
- Filter to one or more tables with `-t`
- Format auto-detected from output file extension
- Optional progress bar (byte-progress for files, spinner for stdin)
- Parquet schema inferred from `CREATE TABLE` or from the first N rows

## Installation

### Linux / macOS

```sh
curl -fsSL https://raw.githubusercontent.com/aodihis/skim/master/scripts/install.sh | sh
```

Installs to `~/.local/bin/skim`. Make sure that directory is in your `PATH`:

```sh
export PATH="$HOME/.local/bin:$PATH"
```

### Windows (PowerShell)

```powershell
irm https://raw.githubusercontent.com/aodihis/skim/master/scripts/install.ps1 | iex
```

Installs to `%USERPROFILE%\.local\bin\skim.exe`.

### From source

```sh
cargo install --path .
```

## Usage

```
skim [OPTIONS] [INPUT]
```

| Argument | Description |
|----------|-------------|
| `INPUT` | SQL dump file. Omit or use `-` to read from stdin. |
| `-o, --output <FILE>` | Output file. Omit or use `-` for stdout (default: stdout). |
| `-f, --format <FORMAT>` | Override output format (see formats below). |
| `-t, --table <TABLE>` | Only convert rows from this table. Repeatable. |
| `--no-header` | Suppress the CSV header row. |
| `--progress` | Show a progress bar on stderr. |
| `--infer-rows <N>` | Rows to buffer for Parquet schema inference when no `CREATE TABLE` is present (default: 1000). |
| `--batch-size <N>` | Rows per Arrow RecordBatch for Parquet output (default: 10000). |
| `--max-statement-size <BYTES>` | Abort if a single SQL statement exceeds this size (default: 256 MiB). |

## Output formats

The format is resolved in this order:
1. `--format` flag (explicit override)
2. Output file extension
3. Default → JSON

| Format | Flag / Extension | Notes |
|--------|-----------------|-------|
| JSON | `--format json` / `.json` | JSON array of objects |
| JSONL | `--format jsonl` / `.jsonl` | One JSON object per line |
| CSV | `--format csv` / `.csv` | Header row + comma-separated values. `NULL` → empty field. |
| YAML | `--format yaml` / `.yaml`, `.yml` | Multi-document YAML, one `---` document per row |
| TOML | `--format toml` / `.toml` | Array of tables |
| Parquet | `--format parquet` / `.parquet` | Requires a real file path (cannot stream to stdout) |

## Examples

Convert a dump to JSONL, streaming from stdin:

```sh
zcat dump.sql.gz | skim --format jsonl > rows.jsonl
```

Extract only the `users` table to CSV:

```sh
skim -t users dump.sql -o users.csv
```

Filter multiple tables, output to stdout as JSON:

```sh
skim -t users -t orders dump.sql
```

Write Parquet with a progress bar:

```sh
skim --progress dump.sql -o output.parquet
```

Pipe through `jq`:

```sh
skim dump.sql | jq '.[] | select(.active == true)'
```

## License

MIT
