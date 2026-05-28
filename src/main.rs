mod cli;
mod error;
mod parser;
mod progress;
mod writer;

use std::{fs::File, io::BufReader};

use parser::{
    schema::extract_schema,
    state_machine::StatementExtractor,
    value_parser::extract_rows,
    Schema,
};

fn main() -> anyhow::Result<()> {
    // Quick debug runner: skim <file.sql>
    // Streams the file, prints every parsed row to stdout.
    // (Full CLI wiring comes in Phase 6.)
    let path = std::env::args().nth(1)
        .unwrap_or_else(|| { eprintln!("Usage: skim <file.sql>"); std::process::exit(1); });

    let file = File::open(&path)?;
    let reader = BufReader::new(file);
    let extractor = StatementExtractor::new(reader, 256 * 1024 * 1024);

    let mut schema = Schema { table_name: String::new(), columns: vec![] };
    let mut row_count = 0usize;

    for stmt_result in extractor {
        let stmt = stmt_result?;

        // If it's a CREATE TABLE, capture the schema and show column names.
        if let Some(s) = extract_schema(&stmt)? {
            schema = s;
            let col_names: Vec<&str> = schema.columns.iter().map(|c| c.name.as_str()).collect();
            println!("─── table: {}  columns: [{}]", schema.table_name, col_names.join(", "));
            continue;
        }

        // If it's an INSERT, parse and print each row.
        let rows = extract_rows(&stmt, &schema)?;
        for row in rows {
            row_count += 1;

            // Pair values with column names when we have a schema.
            let parts: Vec<String> = if schema.columns.is_empty() {
                row.values
                    .iter()
                    .enumerate()
                    .map(|(i, v)| format!("col{i}={v}"))
                    .collect()
            } else {
                schema.columns
                    .iter()
                    .zip(row.values.iter())
                    .map(|(col, val)| format!("{}={}", col.name, val))
                    .collect()
            };

            println!("[{row_count:>6}] {}", parts.join("  |  "));
        }
    }

    println!("\n── {} row(s) total", row_count);
    Ok(())
}
