//! Parse a single complete INSERT statement into a list of rows.
//!
//! # References
//! - sqlparser AST: <https://docs.rs/sqlparser/latest/sqlparser/ast/index.html>
//! - Parser entry point: <https://docs.rs/sqlparser/latest/sqlparser/parser/struct.Parser.html#method.parse_sql>

use sqlparser::ast::{Expr, SetExpr, Statement, TableObject, UnaryOperator};
use sqlparser::ast::Value as SqlValue;
use sqlparser::dialect::{MySqlDialect, PostgreSqlDialect};
use sqlparser::parser::Parser;

use super::{Row, Schema, SqlDialect, Value};

// ── Public API ────────────────────────────────────────────────────────────────

/// Parse a single complete SQL statement string.
///
/// - If it is an `INSERT INTO ... VALUES ...`, returns the extracted rows.
/// - If it is any other statement (`SET`, `CREATE`, `LOCK`, etc.), returns
///   an empty `Vec` — the caller decides what to do with non-INSERT statements.
///
/// `schema` is used only to know the expected column count when the INSERT
/// does **not** list column names (e.g. `INSERT INTO t VALUES (1, 2, 3)`).
/// Pass a schema with empty `columns` if you have no schema yet — the
/// function will skip the column-count check.
pub fn extract_rows(sql: &str, schema: &Schema, dialect: SqlDialect) -> anyhow::Result<Vec<Row>> {
    let stmts = match dialect {
        SqlDialect::Mysql    => Parser::parse_sql(&MySqlDialect {},    sql)?,
        SqlDialect::Postgres => Parser::parse_sql(&PostgreSqlDialect {}, sql)?,
    };

    // The state machine yields one statement at a time, so stmts has ≤ 1 element.
    let Some(stmt) = stmts.into_iter().next() else {
        return Ok(vec![]);
    };

    // We only handle INSERT; everything else is silently ignored.
    let Statement::Insert(insert) = stmt else {
        return Ok(vec![]);
    };

    // Expected column count: from the INSERT column list, or from the schema.
    let expected_cols = if !insert.columns.is_empty() {
        insert.columns.len()
    } else {
        schema.column_count() // 0 if schema is empty → skip check
    };

    // Build a remapping table: schema_index → insert_index.
    //
    // Problem: INSERT may list columns in a different order than CREATE TABLE.
    //   CREATE TABLE users (id, name, email)    ← schema order
    //   INSERT INTO users (email, id, name) ... ← different order!
    //
    // If we stored values in INSERT order and paired them with schema columns
    // by position, every value would be mapped to the wrong column.
    //
    // Solution: when we know both orderings, build a lookup so each schema
    // column gets the value from the correct INSERT position.
    //
    // remap[schema_idx] = insert_idx, or None if the column is absent.
    let remap: Option<Vec<Option<usize>>> =
        if !insert.columns.is_empty() && schema.column_count() > 0 {
            // Lowercase names from the INSERT column list for case-insensitive matching.
            // ObjectName::to_string() includes quote chars (e.g. "`id`", `"id"`).
            // Strip backticks and double-quotes so the name matches the bare
            // column names stored in the schema.
            let insert_names: Vec<String> = insert
                .columns
                .iter()
                .map(|c| c.to_string().trim_matches(&['`', '"'][..]).to_lowercase())
                .collect();

            Some(
                schema
                    .columns
                    .iter()
                    .map(|sc| {
                        insert_names
                            .iter()
                            .position(|n| n == &sc.name.to_lowercase())
                    })
                    .collect(),
            )
        } else {
            None // no remapping needed: either INSERT has no column list,
                 // or we have no schema — use values as-is
        };

    // Navigate: Insert.source → Query.body → SetExpr::Values → Values.rows
    let source = match insert.source {
        Some(s) => s,
        None    => return Ok(vec![]), // INSERT ... SET form (MySQL), no VALUES
    };

    let values = match source.body.as_ref() {
        SetExpr::Values(v) => v,
        _                  => return Ok(vec![]),
    };

    // Convert each row of SQL expressions into our Row type.
    let mut rows = Vec::with_capacity(values.rows.len());
    for raw_row in &values.rows {
        if expected_cols > 0 && raw_row.len() != expected_cols {
            anyhow::bail!(
                "row has {} values but expected {} columns (table: {})",
                raw_row.len(),
                expected_cols,
                insert.table,
            );
        }

        // Convert every expression in INSERT order first.
        let insert_values = raw_row
            .iter()
            .map(expr_to_value)
            .collect::<anyhow::Result<Vec<_>>>()?;

        // Then reorder to schema order (if we built a remap table).
        let final_values = if let Some(ref remap) = remap {
            remap
                .iter()
                .map(|maybe_idx| match maybe_idx {
                    Some(i) => insert_values[*i].clone(),
                    None    => Value::Null, // column not present in INSERT → NULL
                })
                .collect()
        } else {
            insert_values
        };

        rows.push(Row { values: final_values });
    }

    Ok(rows)
}

// ── Expr → Value ──────────────────────────────────────────────────────────────

/// Convert a single sqlparser `Expr` leaf into our `Value` enum.
fn expr_to_value(expr: &Expr) -> anyhow::Result<Value> {
    match expr {
        // The normal case: a literal value.
        // In sqlparser 0.62, Expr::Value holds a ValueWithSpan; `.value` is the field.
        Expr::Value(v) => sql_value_to_value(&v.value),

        // Negative numbers: `-42` is parsed as UnaryOp(Minus, Number("42")).
        Expr::UnaryOp { op: UnaryOperator::Minus, expr } => {
            match expr_to_value(expr)? {
                Value::Integer(n) => Ok(Value::Integer(-n)),
                Value::Float(f)   => Ok(Value::Float(-f)),
                other             => Ok(other),
            }
        }

        // DEFAULT keyword (e.g. INSERT INTO t VALUES (DEFAULT, 'foo')).
        Expr::Identifier(ident) if ident.value.eq_ignore_ascii_case("default") => {
            Ok(Value::Null)
        }

        // PostgreSQL cast: expr::TYPE — extract the value, discard the cast.
        // e.g. '2024-01-01'::timestamp  →  Value::Text("2024-01-01")
        //      42::bigint               →  Value::Integer(42)
        Expr::Cast { expr, .. } => expr_to_value(expr),

        // Typed string literals: TIMESTAMP '2024-01-01' — extract the inner value.
        Expr::TypedString(ts) => sql_value_to_value(&ts.value.value),

        // Anything else we don't recognise: stringify it as text.
        other => Ok(Value::Text(other.to_string())),
    }
}

/// Convert a sqlparser `Value` literal into our `Value` enum.
fn sql_value_to_value(v: &SqlValue) -> anyhow::Result<Value> {
    match v {
        SqlValue::Null          => Ok(Value::Null),
        SqlValue::Boolean(b)    => Ok(Value::Bool(*b)),

        // Numbers: try integer first, then float.
        SqlValue::Number(n, _)  => {
            if let Ok(i) = n.parse::<i64>() {
                Ok(Value::Integer(i))
            } else {
                Ok(Value::Float(n.parse::<f64>()?))
            }
        }

        // Hex literals: X'DEADBEEF' — sqlparser gives us just "DEADBEEF".
        SqlValue::HexStringLiteral(h) => Ok(Value::Bytes(decode_hex(h))),

        // All string variants — sqlparser already handled unescaping.
        SqlValue::SingleQuotedString(s)
        | SqlValue::DoubleQuotedString(s)
        | SqlValue::EscapedStringLiteral(s)
        | SqlValue::NationalStringLiteral(s)
        | SqlValue::UnicodeStringLiteral(s)
        | SqlValue::TripleSingleQuotedString(s)
        | SqlValue::TripleDoubleQuotedString(s)
        | SqlValue::SingleQuotedRawStringLiteral(s)
        | SqlValue::DoubleQuotedRawStringLiteral(s)
        | SqlValue::TripleSingleQuotedRawStringLiteral(s)
        | SqlValue::TripleDoubleQuotedRawStringLiteral(s)
        | SqlValue::SingleQuotedByteStringLiteral(s)
        | SqlValue::DoubleQuotedByteStringLiteral(s)
        | SqlValue::TripleSingleQuotedByteStringLiteral(s)
        | SqlValue::TripleDoubleQuotedByteStringLiteral(s) => Ok(Value::Text(s.clone())),

        // Dollar-quoted strings (PostgreSQL): $$text$$ or $tag$text$tag$
        SqlValue::DollarQuotedString(dqs) => Ok(Value::Text(dqs.value.clone())),

        // Fallback: convert whatever it is to its display string.
        other => Ok(Value::Text(other.to_string())),
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Decode a hex string like "DEADBEEF" into bytes.
/// Invalid nibbles are silently skipped.
fn decode_hex(s: &str) -> Vec<u8> {
    let s = s.trim();
    let s = if s.len() % 2 != 0 { &s[1..] } else { s }; // drop odd leading nibble
    (0..s.len())
        .step_by(2)
        .filter_map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

/// Return the table name from an INSERT statement, or `None` if the SQL is
/// not an INSERT (SET, LOCK, CREATE TABLE, etc. all return `None`).
///
/// Used by the main pipeline to filter statements by table name before
/// doing the full row-extraction parse.
pub fn insert_table_name(sql: &str, dialect: SqlDialect) -> anyhow::Result<Option<String>> {
    let stmts = match dialect {
        SqlDialect::Mysql    => Parser::parse_sql(&MySqlDialect {},    sql)?,
        SqlDialect::Postgres => Parser::parse_sql(&PostgreSqlDialect {}, sql)?,
    };
    let Some(stmt) = stmts.into_iter().next() else { return Ok(None); };
    let Statement::Insert(insert) = stmt else { return Ok(None); };
    Ok(table_name_from_object(&insert.table))
}

/// Extract the unqualified table name from a `TableObject`.
/// Strips any schema prefix: `public.users` → `users`.
/// Returns `None` for non-simple-name forms (table functions, sub-queries).
pub fn table_name_from_object(obj: &TableObject) -> Option<String> {
    match obj {
        TableObject::TableName(name) => {
            // ObjectName::to_string() gives "schema.table" or just "table".
            // Split on '.' and take the last segment, stripping any wrapping quotes.
            let full = name.to_string();
            let unqualified = full.split('.').last().unwrap_or(&full);
            Some(unqualified.trim_matches('"').trim_matches('`').to_string())
        }
        _ => None,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{Column, InferredType, Schema};

    /// Build an empty schema (no column info — skips column-count check).
    fn no_schema() -> Schema {
        Schema { table_name: "t".into(), columns: vec![] }
    }

    /// Build a schema with the given column names.
    fn schema(cols: &[&str]) -> Schema {
        Schema {
            table_name: "t".into(),
            columns: cols.iter().map(|n| Column {
                name: n.to_string(),
                inferred_type: InferredType::Unknown,
            }).collect(),
        }
    }

    // ── Basic INSERT parsing ────────────────────────────────────────────────

    #[test]
    fn single_row_with_column_list() {
        let sql = "INSERT INTO users (id, name, age) VALUES (1, 'Alice', 30)";
        let rows = extract_rows(sql, &no_schema(), SqlDialect::Mysql).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].values[0], Value::Integer(1));
        assert_eq!(rows[0].values[1], Value::Text("Alice".into()));
        assert_eq!(rows[0].values[2], Value::Integer(30));
    }

    #[test]
    fn multi_row_values() {
        // One INSERT with three tuples in VALUES.
        let sql = "INSERT INTO t (a, b) VALUES (1, 'x'), (2, 'y'), (3, 'z')";
        let rows = extract_rows(sql, &no_schema(), SqlDialect::Mysql).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[2].values[0], Value::Integer(3));
        assert_eq!(rows[2].values[1], Value::Text("z".into()));
    }

    // ── Value types ─────────────────────────────────────────────────────────

    #[test]
    fn null_value() {
        let sql = "INSERT INTO t (a, b) VALUES (NULL, 'hello')";
        let rows = extract_rows(sql, &no_schema(), SqlDialect::Mysql).unwrap();
        assert_eq!(rows[0].values[0], Value::Null);
        assert_eq!(rows[0].values[1], Value::Text("hello".into()));
    }

    #[test]
    fn boolean_values() {
        let sql = "INSERT INTO t (a, b) VALUES (TRUE, FALSE)";
        let rows = extract_rows(sql, &no_schema(), SqlDialect::Mysql).unwrap();
        assert_eq!(rows[0].values[0], Value::Bool(true));
        assert_eq!(rows[0].values[1], Value::Bool(false));
    }

    #[test]
    fn negative_integer() {
        let sql = "INSERT INTO t (n) VALUES (-42)";
        let rows = extract_rows(sql, &no_schema(), SqlDialect::Mysql).unwrap();
        assert_eq!(rows[0].values[0], Value::Integer(-42));
    }

    #[test]
    fn negative_float() {
        let sql = "INSERT INTO t (n) VALUES (-3.14)";
        let rows = extract_rows(sql, &no_schema(), SqlDialect::Mysql).unwrap();
        assert_eq!(rows[0].values[0], Value::Float(-3.14));
    }

    #[test]
    fn positive_float() {
        let sql = "INSERT INTO t (price) VALUES (9.99)";
        let rows = extract_rows(sql, &no_schema(), SqlDialect::Mysql).unwrap();
        assert_eq!(rows[0].values[0], Value::Float(9.99));
    }

    #[test]
    fn large_integer_max() {
        let sql = "INSERT INTO t (n) VALUES (9223372036854775807)"; // i64::MAX
        let rows = extract_rows(sql, &no_schema(), SqlDialect::Mysql).unwrap();
        assert_eq!(rows[0].values[0], Value::Integer(i64::MAX));
    }

    // ── Strings ─────────────────────────────────────────────────────────────

    #[test]
    fn string_with_doubled_quote_escape() {
        // SQL standard: 'it''s fine' → "it's fine" (sqlparser unescapes this).
        let sql = "INSERT INTO t (msg) VALUES ('it''s fine')";
        let rows = extract_rows(sql, &no_schema(), SqlDialect::Mysql).unwrap();
        assert_eq!(rows[0].values[0], Value::Text("it's fine".into()));
    }

    #[test]
    fn empty_string() {
        let sql = "INSERT INTO t (s) VALUES ('')";
        let rows = extract_rows(sql, &no_schema(), SqlDialect::Mysql).unwrap();
        assert_eq!(rows[0].values[0], Value::Text("".into()));
    }

    // ── Hex / binary ────────────────────────────────────────────────────────

    #[test]
    fn hex_literal() {
        let sql = "INSERT INTO t (data) VALUES (X'DEADBEEF')";
        let rows = extract_rows(sql, &no_schema(), SqlDialect::Mysql).unwrap();
        assert_eq!(rows[0].values[0], Value::Bytes(vec![0xDE, 0xAD, 0xBE, 0xEF]));
    }

    // ── Special keywords ────────────────────────────────────────────────────

    #[test]
    fn default_keyword_becomes_null() {
        let sql = "INSERT INTO t (a, b) VALUES (DEFAULT, 'foo')";
        let rows = extract_rows(sql, &no_schema(), SqlDialect::Mysql).unwrap();
        assert_eq!(rows[0].values[0], Value::Null);
        assert_eq!(rows[0].values[1], Value::Text("foo".into()));
    }

    // ── Non-INSERT statements ───────────────────────────────────────────────

    #[test]
    fn set_statement_returns_empty() {
        let sql = "SET NAMES utf8mb4";
        let rows = extract_rows(sql, &no_schema(), SqlDialect::Mysql).unwrap();
        assert!(rows.is_empty(), "SET should return empty vec");
    }

    #[test]
    fn create_table_returns_empty() {
        let sql = "CREATE TABLE t (id INT, name TEXT)";
        let rows = extract_rows(sql, &no_schema(), SqlDialect::Mysql).unwrap();
        assert!(rows.is_empty(), "CREATE TABLE should return empty vec");
    }

    // ── Schema column-count check ───────────────────────────────────────────

    #[test]
    fn column_count_mismatch_errors() {
        // Schema has 2 columns but the INSERT has 3 values — must error.
        let sql = "INSERT INTO t VALUES (1, 2, 3)";
        let s = schema(&["a", "b"]);
        let result = extract_rows(sql, &s, SqlDialect::Mysql);
        assert!(result.is_err(), "column count mismatch should return an error");
    }

    #[test]
    fn no_schema_skips_count_check() {
        // No schema → no column-count check → any number of values is OK.
        let sql = "INSERT INTO t VALUES (1, 2, 3, 4, 5)";
        let rows = extract_rows(sql, &no_schema(), SqlDialect::Mysql).unwrap();
        assert_eq!(rows[0].values.len(), 5);
    }

    // ── decode_hex unit tests ───────────────────────────────────────────────

    #[test]
    fn decode_hex_basic() {
        assert_eq!(decode_hex("DEADBEEF"), vec![0xDE, 0xAD, 0xBE, 0xEF]);
        assert_eq!(decode_hex("00FF"),     vec![0x00, 0xFF]);
        assert_eq!(decode_hex(""),         Vec::<u8>::new());
    }

    #[test]
    fn decode_hex_lowercase() {
        assert_eq!(decode_hex("deadbeef"), vec![0xDE, 0xAD, 0xBE, 0xEF]);
    }

    // ── Column reordering ───────────────────────────────────────────────────

    #[test]
    fn insert_columns_different_order_than_schema() {
        // Schema: id=0, name=1, email=2
        // INSERT column list: email, id, name  (different order!)
        // Values:             'a',   1,  'Bob'
        // After remapping, row should be in schema order: 1, 'Bob', 'a'
        let s = schema(&["id", "name", "email"]);
        let sql = "INSERT INTO users (email, id, name) VALUES ('alice@x.com', 1, 'Alice')";
        let rows = extract_rows(sql, &s, SqlDialect::Mysql).unwrap();

        assert_eq!(rows[0].values[0], Value::Integer(1),            "id");
        assert_eq!(rows[0].values[1], Value::Text("Alice".into()),  "name");
        assert_eq!(rows[0].values[2], Value::Text("alice@x.com".into()), "email");
    }

    #[test]
    fn insert_columns_same_order_as_schema_unchanged() {
        // When orders match, remapping should be a no-op.
        let s = schema(&["id", "name", "email"]);
        let sql = "INSERT INTO users (id, name, email) VALUES (42, 'Bob', 'bob@x.com')";
        let rows = extract_rows(sql, &s, SqlDialect::Mysql).unwrap();

        assert_eq!(rows[0].values[0], Value::Integer(42));
        assert_eq!(rows[0].values[1], Value::Text("Bob".into()));
        assert_eq!(rows[0].values[2], Value::Text("bob@x.com".into()));
    }

    #[test]
    fn insert_missing_column_becomes_null() {
        // Schema has 3 cols; INSERT only provides 2 of them.
        // The missing schema column should become NULL.
        let s = schema(&["id", "name", "email"]);
        let sql = "INSERT INTO users (id, name) VALUES (7, 'Carol')";
        let rows = extract_rows(sql, &s, SqlDialect::Mysql).unwrap();

        assert_eq!(rows[0].values[0], Value::Integer(7));
        assert_eq!(rows[0].values[1], Value::Text("Carol".into()));
        assert_eq!(rows[0].values[2], Value::Null, "missing email should be NULL");
    }

    // ── PostgreSQL dialect ──────────────────────────────────────────────────

    #[test]
    fn pg_double_quoted_identifiers() {
        // pg_dump uses double-quoted identifiers.
        let sql = r#"INSERT INTO "users" ("id", "name") VALUES (1, 'Alice')"#;
        let rows = extract_rows(sql, &no_schema(), SqlDialect::Postgres).unwrap();
        assert_eq!(rows[0].values[0], Value::Integer(1));
        assert_eq!(rows[0].values[1], Value::Text("Alice".into()));
    }

    #[test]
    fn pg_schema_qualified_table_name() {
        // pg_dump writes INSERT INTO public.users — the schema prefix must be
        // stripped so that -t users matches.
        let sql = "INSERT INTO public.users (id, name) VALUES (1, 'Alice')";
        let name = insert_table_name(sql, SqlDialect::Postgres).unwrap();
        assert_eq!(name.as_deref(), Some("users"));
    }

    #[test]
    fn pg_dollar_quoted_string() {
        // PostgreSQL $$ quoting — sqlparser returns the inner text.
        let sql = "INSERT INTO t (msg) VALUES ($$hello world$$)";
        let rows = extract_rows(sql, &no_schema(), SqlDialect::Postgres).unwrap();
        assert_eq!(rows[0].values[0], Value::Text("hello world".into()));
    }

    #[test]
    fn pg_cast_discards_type_annotation() {
        // '2024-01-01'::timestamp — cast is stripped, inner string value kept.
        let sql = "INSERT INTO t (created_at) VALUES ('2024-01-01'::timestamp)";
        let rows = extract_rows(sql, &no_schema(), SqlDialect::Postgres).unwrap();
        assert_eq!(rows[0].values[0], Value::Text("2024-01-01".into()));
    }

    #[test]
    fn pg_cast_on_integer() {
        // 42::bigint — cast is stripped, integer value kept.
        let sql = "INSERT INTO t (n) VALUES (42::bigint)";
        let rows = extract_rows(sql, &no_schema(), SqlDialect::Postgres).unwrap();
        assert_eq!(rows[0].values[0], Value::Integer(42));
    }

    #[test]
    fn pg_null_value() {
        let sql = "INSERT INTO t (a, b) VALUES (NULL, 'hello')";
        let rows = extract_rows(sql, &no_schema(), SqlDialect::Postgres).unwrap();
        assert_eq!(rows[0].values[0], Value::Null);
        assert_eq!(rows[0].values[1], Value::Text("hello".into()));
    }

    #[test]
    fn pg_boolean_values() {
        let sql = "INSERT INTO t (a, b) VALUES (TRUE, FALSE)";
        let rows = extract_rows(sql, &no_schema(), SqlDialect::Postgres).unwrap();
        assert_eq!(rows[0].values[0], Value::Bool(true));
        assert_eq!(rows[0].values[1], Value::Bool(false));
    }

    #[test]
    fn pg_multi_row_values() {
        let sql = "INSERT INTO t (id, val) VALUES (1, 'x'), (2, 'y'), (3, 'z')";
        let rows = extract_rows(sql, &no_schema(), SqlDialect::Postgres).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[2].values[0], Value::Integer(3));
        assert_eq!(rows[2].values[1], Value::Text("z".into()));
    }

    #[test]
    fn pg_negative_number() {
        let sql = "INSERT INTO t (n) VALUES (-99)";
        let rows = extract_rows(sql, &no_schema(), SqlDialect::Postgres).unwrap();
        assert_eq!(rows[0].values[0], Value::Integer(-99));
    }
}
