//! Parse a single complete INSERT statement into a list of rows.
//!
//! # References
//! - sqlparser AST: <https://docs.rs/sqlparser/latest/sqlparser/ast/index.html>
//! - Parser entry point: <https://docs.rs/sqlparser/latest/sqlparser/parser/struct.Parser.html#method.parse_sql>

use memchr::memchr2;
use sqlparser::ast::{Expr, Insert, SetExpr, Statement, TableObject, UnaryOperator};
use sqlparser::ast::Value as SqlValue;
use sqlparser::dialect::{MySqlDialect, PostgreSqlDialect};
use sqlparser::parser::Parser;

use super::{Row, Schema, SqlDialect, Value};
use super::schema::unqualified_name;

// ── Public API ────────────────────────────────────────────────────────────────

/// Parsed data from a single `INSERT` statement.
pub struct ParsedInsertRows {
    pub table_name: Option<String>,
    pub rows: Vec<Row>,
    pub used_fast_path: bool,
}

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
#[cfg_attr(not(test), allow(dead_code))]
pub fn extract_rows(sql: &str, schema: &Schema, dialect: SqlDialect) -> anyhow::Result<Vec<Row>> {
    Ok(extract_insert_rows(sql, schema, dialect)?
        .map(|insert| insert.rows)
        .unwrap_or_default())
}

/// Parse one SQL statement and return both the target table name and rows when
/// it is an `INSERT INTO ... VALUES ...` statement.
pub fn extract_insert_rows(
    sql: &str,
    schema: &Schema,
    dialect: SqlDialect,
) -> anyhow::Result<Option<ParsedInsertRows>> {
    if dialect == SqlDialect::Mysql {
        if let Some(parsed) = parse_mysql_insert_fast(sql, schema)? {
            return Ok(Some(parsed));
        }
    }

    let Some(insert) = parse_insert(sql, dialect)? else {
        return Ok(None);
    };
    let table_name = table_name_from_object(&insert.table);
    let rows = rows_from_insert(insert, schema)?;
    Ok(Some(ParsedInsertRows {
        table_name,
        rows,
        used_fast_path: false,
    }))
}

/// Parse one SQL statement into a `sqlparser` INSERT AST and immediately drop
/// it. Used by performance profiling to isolate AST construction cost from row
/// conversion and output writing.
pub fn parse_insert_ast_only(sql: &str, dialect: SqlDialect) -> anyhow::Result<bool> {
    Ok(parse_insert(sql, dialect)?.is_some())
}

fn rows_from_insert(insert: Insert, schema: &Schema) -> anyhow::Result<Vec<Row>> {
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

            if insert_names.len() == schema.columns.len()
                && insert_names
                    .iter()
                    .zip(schema.columns.iter())
                    .all(|(insert, schema_col)| insert.eq_ignore_ascii_case(&schema_col.name))
            {
                None
            } else {
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
            }
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

fn parse_mysql_insert_fast(
    sql: &str,
    schema: &Schema,
) -> anyhow::Result<Option<ParsedInsertRows>> {
    let mut p = MysqlInsertParser::new(sql);
    let Some(parsed) = p.parse(schema)? else {
        return Ok(None);
    };
    Ok(Some(parsed))
}

struct MysqlInsertParser<'a> {
    sql: &'a str,
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> MysqlInsertParser<'a> {
    fn new(sql: &'a str) -> Self {
        Self {
            sql,
            bytes: sql.as_bytes(),
            pos: 0,
        }
    }

    fn parse(&mut self, schema: &Schema) -> anyhow::Result<Option<ParsedInsertRows>> {
        self.skip_ws();
        if !self.consume_keyword("insert") {
            return Ok(None);
        }

        loop {
            self.skip_ws();
            if self.consume_keyword("low_priority")
                || self.consume_keyword("delayed")
                || self.consume_keyword("high_priority")
                || self.consume_keyword("ignore")
            {
                continue;
            }
            break;
        }

        self.skip_ws();
        if !self.consume_keyword("into") {
            return Ok(None);
        }

        self.skip_ws();
        let Some(table_name) = self.parse_object_name() else {
            return Ok(None);
        };

        self.skip_ws();
        let insert_columns = if self.peek() == Some(b'(') {
            let Some(columns) = self.parse_column_list() else {
                return Ok(None);
            };
            Some(columns)
        } else {
            None
        };

        self.skip_ws();
        if !self.consume_keyword("values") && !self.consume_keyword("value") {
            return Ok(None);
        }

        let remap = build_remap(insert_columns.as_deref(), schema);
        let expected_cols = insert_columns
            .as_ref()
            .map(|cols| cols.len())
            .unwrap_or_else(|| schema.column_count());

        let mut rows = Vec::new();
        loop {
            self.skip_ws();
            if self.eof() || self.peek() == Some(b';') {
                break;
            }
            if self.peek() != Some(b'(') {
                return Ok(None);
            }
            self.pos += 1;

            let mut insert_values = Vec::new();
            loop {
                self.skip_ws();
                let Some(value) = self.parse_value()? else {
                    return Ok(None);
                };
                insert_values.push(value);
                self.skip_ws();
                match self.peek() {
                    Some(b',') => self.pos += 1,
                    Some(b')') => {
                        self.pos += 1;
                        break;
                    }
                    _ => return Ok(None),
                }
            }

            if expected_cols > 0 && insert_values.len() != expected_cols {
                anyhow::bail!(
                    "row has {} values but expected {} columns (table: {})",
                    insert_values.len(),
                    expected_cols,
                    table_name,
                );
            }

            let values = apply_remap(insert_values, remap.as_deref());
            rows.push(Row { values });

            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b';') => break,
                None => break,
                _ => return Ok(None),
            }
        }

        Ok(Some(ParsedInsertRows {
            table_name: Some(table_name),
            rows,
            used_fast_path: true,
        }))
    }

    fn parse_object_name(&mut self) -> Option<String> {
        let mut last = self.parse_ident()?;
        loop {
            self.skip_ws();
            if self.peek() != Some(b'.') {
                return Some(last);
            }
            self.pos += 1;
            self.skip_ws();
            last = self.parse_ident()?;
        }
    }

    fn parse_column_list(&mut self) -> Option<Vec<String>> {
        if self.peek() != Some(b'(') {
            return None;
        }
        self.pos += 1;
        let mut cols = Vec::new();
        loop {
            self.skip_ws();
            if self.peek() == Some(b')') {
                self.pos += 1;
                return Some(cols);
            }
            cols.push(self.parse_ident()?);
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b')') => {
                    self.pos += 1;
                    return Some(cols);
                }
                _ => return None,
            }
        }
    }

    fn parse_ident(&mut self) -> Option<String> {
        match self.peek()? {
            b'`' => self.parse_backtick_ident(),
            b'"' => self.parse_double_quote_ident(),
            _ => self.parse_bare_ident(),
        }
    }

    fn parse_backtick_ident(&mut self) -> Option<String> {
        self.pos += 1;
        let mut out = Vec::new();
        while !self.eof() {
            let b = self.bytes[self.pos];
            self.pos += 1;
            if b == b'`' {
                if self.peek() == Some(b'`') {
                    out.push(b'`');
                    self.pos += 1;
                } else {
                    return Some(String::from_utf8_lossy(&out).into_owned());
                }
            } else {
                out.push(b);
            }
        }
        None
    }

    fn parse_double_quote_ident(&mut self) -> Option<String> {
        self.pos += 1;
        let mut out = Vec::new();
        while !self.eof() {
            let b = self.bytes[self.pos];
            self.pos += 1;
            if b == b'"' {
                return Some(String::from_utf8_lossy(&out).into_owned());
            }
            out.push(b);
        }
        None
    }

    fn parse_bare_ident(&mut self) -> Option<String> {
        let start = self.pos;
        while !self.eof() {
            let b = self.bytes[self.pos];
            if b.is_ascii_alphanumeric() || matches!(b, b'_' | b'$') {
                self.pos += 1;
            } else {
                break;
            }
        }
        if self.pos == start {
            return None;
        }
        Some(self.sql[start..self.pos].to_string())
    }

    fn parse_value(&mut self) -> anyhow::Result<Option<Value>> {
        match self.peek() {
            Some(b'\'') => Ok(Some(Value::Text(self.parse_single_quoted_string()?))),
            Some(b'X') | Some(b'x') if self.peek_at(1) == Some(b'\'') => {
                self.pos += 1;
                let hex = self.parse_single_quoted_string()?;
                Ok(Some(Value::Bytes(decode_hex(&hex))))
            }
            Some(b'+') | Some(b'-') | Some(b'0'..=b'9') => self.parse_number(),
            Some(_) if self.consume_keyword("null") => Ok(Some(Value::Null)),
            Some(_) if self.consume_keyword("default") => Ok(Some(Value::Null)),
            Some(_) if self.consume_keyword("true") => Ok(Some(Value::Bool(true))),
            Some(_) if self.consume_keyword("false") => Ok(Some(Value::Bool(false))),
            _ => Ok(None),
        }
    }

    fn parse_single_quoted_string(&mut self) -> anyhow::Result<String> {
        if self.peek() != Some(b'\'') {
            anyhow::bail!("expected quoted string");
        }
        self.pos += 1;
        let content_start = self.pos;
        let mut out: Option<Vec<u8>> = None;
        while !self.eof() {
            let rest = &self.bytes[self.pos..];
            let Some(next_special) = memchr2(b'\'', b'\\', rest) else {
                anyhow::bail!("unterminated quoted string");
            };
            if let Some(out) = out.as_mut() {
                out.extend_from_slice(&rest[..next_special]);
            }
            self.pos += next_special;

            let b = self.bytes[self.pos];
            self.pos += 1;
            match b {
                b'\'' => {
                    if self.peek() == Some(b'\'') {
                        let out = out.get_or_insert_with(|| {
                            Vec::from(&self.bytes[content_start..self.pos - 1])
                        });
                        out.push(b'\'');
                        self.pos += 1;
                    } else {
                        let content_end = self.pos - 1;
                        return match out {
                            Some(out) => Ok(String::from_utf8_lossy(&out).into_owned()),
                            None => Ok(self.sql[content_start..content_end].to_string()),
                        };
                    }
                }
                b'\\' => {
                    let out = out.get_or_insert_with(|| {
                        Vec::from(&self.bytes[content_start..self.pos - 1])
                    });
                    if self.eof() {
                        out.push(b'\\');
                        break;
                    }
                    let escaped = self.bytes[self.pos];
                    self.pos += 1;
                    out.push(match escaped {
                        b'0' => 0,
                        b'\'' => b'\'',
                        b'"' => b'"',
                        b'b' => 0x08,
                        b'n' => b'\n',
                        b'r' => b'\r',
                        b't' => b'\t',
                        b'Z' => 0x1A,
                        b'\\' => b'\\',
                        other => other,
                    });
                }
                _ => unreachable!("memchr2 only finds quotes and backslashes"),
            }
        }
        anyhow::bail!("unterminated quoted string")
    }

    fn parse_number(&mut self) -> anyhow::Result<Option<Value>> {
        let start = self.pos;
        if matches!(self.peek(), Some(b'+' | b'-')) {
            self.pos += 1;
        }
        let mut has_digit = false;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            has_digit = true;
            self.pos += 1;
        }
        let mut is_float = false;
        if self.peek() == Some(b'.') {
            is_float = true;
            self.pos += 1;
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                has_digit = true;
                self.pos += 1;
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            is_float = true;
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            let exp_start = self.pos;
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
            if self.pos == exp_start {
                self.pos = start;
                return Ok(None);
            }
        }
        if !has_digit {
            self.pos = start;
            return Ok(None);
        }
        let s = &self.sql[start..self.pos];
        if is_float {
            Ok(Some(Value::Float(s.parse::<f64>()?)))
        } else if let Ok(n) = s.parse::<i64>() {
            Ok(Some(Value::Integer(n)))
        } else {
            Ok(Some(Value::Float(s.parse::<f64>()?)))
        }
    }

    fn consume_keyword(&mut self, keyword: &str) -> bool {
        let end = self.pos + keyword.len();
        if end > self.bytes.len() {
            return false;
        }
        let candidate = &self.sql[self.pos..end];
        if !candidate.eq_ignore_ascii_case(keyword) {
            return false;
        }
        if self
            .bytes
            .get(end)
            .is_some_and(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'$'))
        {
            return false;
        }
        self.pos = end;
        true
    }

    fn skip_ws(&mut self) {
        while self.peek().is_some_and(|b| b.is_ascii_whitespace()) {
            self.pos += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn peek_at(&self, offset: usize) -> Option<u8> {
        self.bytes.get(self.pos + offset).copied()
    }

    fn eof(&self) -> bool {
        self.pos >= self.bytes.len()
    }
}

fn build_remap(insert_columns: Option<&[String]>, schema: &Schema) -> Option<Vec<Option<usize>>> {
    let insert_columns = insert_columns?;
    if schema.column_count() == 0 {
        return None;
    }
    if insert_columns.len() == schema.columns.len()
        && insert_columns
            .iter()
            .zip(schema.columns.iter())
            .all(|(insert, schema_col)| insert.eq_ignore_ascii_case(&schema_col.name))
    {
        return None;
    }
    Some(
        schema
            .columns
            .iter()
            .map(|sc| {
                insert_columns
                    .iter()
                    .position(|n| n.eq_ignore_ascii_case(&sc.name))
            })
            .collect(),
    )
}

fn apply_remap(insert_values: Vec<Value>, remap: Option<&[Option<usize>]>) -> Vec<Value> {
    let Some(remap) = remap else {
        return insert_values;
    };
    remap
        .iter()
        .map(|maybe_idx| match maybe_idx {
            Some(i) => insert_values[*i].clone(),
            None => Value::Null,
        })
        .collect()
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
    let s = if !s.len().is_multiple_of(2) { &s[1..] } else { s }; // drop odd leading nibble
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
#[cfg_attr(not(test), allow(dead_code))]
pub fn insert_table_name(sql: &str, dialect: SqlDialect) -> anyhow::Result<Option<String>> {
    let Some(insert) = parse_insert(sql, dialect)? else {
        return Ok(None);
    };
    Ok(table_name_from_object(&insert.table))
}

fn parse_insert(sql: &str, dialect: SqlDialect) -> anyhow::Result<Option<Insert>> {
    let stmts = match dialect {
        SqlDialect::Mysql    => Parser::parse_sql(&MySqlDialect {},    sql)?,
        SqlDialect::Postgres => Parser::parse_sql(&PostgreSqlDialect {}, sql)?,
    };

    // The state machine yields one statement at a time, so stmts has ≤ 1 element.
    let Some(stmt) = stmts.into_iter().next() else {
        return Ok(None);
    };

    let Statement::Insert(insert) = stmt else {
        return Ok(None);
    };

    Ok(Some(insert))
}

/// Extract the unqualified table name from a `TableObject`.
/// Strips any schema prefix: `public.users` → `users`.
/// Returns `None` for non-simple-name forms (table functions, sub-queries).
pub fn table_name_from_object(obj: &TableObject) -> Option<String> {
    match obj {
        TableObject::TableName(name) => Some(unqualified_name(name)),
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

    #[test]
    fn extract_insert_rows_returns_table_and_rows() {
        let sql = "INSERT INTO `users` (id, name) VALUES (1, 'Alice'), (2, 'Bob')";
        let parsed = extract_insert_rows(sql, &no_schema(), SqlDialect::Mysql)
            .unwrap()
            .expect("expected insert rows");

        assert_eq!(parsed.table_name.as_deref(), Some("users"));
        assert_eq!(parsed.rows.len(), 2);
        assert_eq!(parsed.rows[0].values[1], Value::Text("Alice".into()));
        assert_eq!(parsed.rows[1].values[0], Value::Integer(2));
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
    fn mysql_fast_path_handles_unescaped_arabic_string() {
        let sql = "INSERT INTO `HadithTable` (`id`, `arabic`) VALUES (1, 'حدثنا عبد الله بن يوسف')";
        let parsed = extract_insert_rows(sql, &no_schema(), SqlDialect::Mysql)
            .unwrap()
            .expect("expected insert rows");

        assert!(parsed.used_fast_path);
        assert_eq!(parsed.table_name.as_deref(), Some("HadithTable"));
        assert_eq!(
            parsed.rows[0].values[1],
            Value::Text("حدثنا عبد الله بن يوسف".into()),
        );
    }

    #[test]
    fn mysql_fast_path_handles_backslash_escapes() {
        let sql = r"INSERT INTO t (msg) VALUES ('line\nquote\'slash\\')";
        let parsed = extract_insert_rows(sql, &no_schema(), SqlDialect::Mysql)
            .unwrap()
            .expect("expected insert rows");

        assert!(parsed.used_fast_path);
        assert_eq!(
            parsed.rows[0].values[0],
            Value::Text("line\nquote'slash\\".into()),
        );
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
    fn same_order_insert_columns_do_not_build_remap() {
        let s = schema(&["id", "name", "email"]);
        let insert_columns = vec!["id".into(), "name".into(), "email".into()];

        assert!(build_remap(Some(&insert_columns), &s).is_none());
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
