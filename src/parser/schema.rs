//! Extract a `Schema` from a `CREATE TABLE` SQL statement.
//!
//! # References
//! - CreateTable AST: <https://docs.rs/sqlparser/latest/sqlparser/ast/struct.CreateTable.html>
//! - DataType enum:   <https://docs.rs/sqlparser/latest/sqlparser/ast/enum.DataType.html>

use sqlparser::ast::{DataType, Statement};
use sqlparser::dialect::MySqlDialect;
use sqlparser::parser::Parser;

use super::{Column, InferredType, Schema};

// ── Public API ────────────────────────────────────────────────────────────────

/// Try to extract a `Schema` from a SQL statement string.
///
/// Returns `Some(Schema)` if the statement is a `CREATE TABLE`.
/// Returns `None` for any other statement (INSERT, SET, etc.).
pub fn extract_schema(sql: &str) -> anyhow::Result<Option<Schema>> {
    let dialect = MySqlDialect {};
    let stmts = Parser::parse_sql(&dialect, sql)?;

    let Some(stmt) = stmts.into_iter().next() else {
        return Ok(None);
    };

    let Statement::CreateTable(ct) = stmt else {
        return Ok(None);
    };

    // ObjectName implements Display — gives "schema.table" or just "table".
    let table_name = ct.name.to_string();

    let columns = ct
        .columns
        .iter()
        .map(|col_def| Column {
            // Ident.value is the bare name without backticks or quotes.
            name: col_def.name.value.clone(),
            inferred_type: data_type_to_inferred(&col_def.data_type),
        })
        .collect();

    Ok(Some(Schema { table_name, columns }))
}

// ── DataType → InferredType ───────────────────────────────────────────────────

/// Map a sqlparser `DataType` to our `InferredType`.
///
/// Used so that when a `CREATE TABLE` precedes the INSERT statements, the
/// Parquet writer gets exact column types instead of relying on inference.
fn data_type_to_inferred(dt: &DataType) -> InferredType {
    match dt {
        // ── Boolean ──────────────────────────────────────────────────────────
        DataType::Boolean | DataType::Bool => InferredType::Boolean,

        // ── Integer family (sqlparser 0.62 variants) ─────────────────────────
        DataType::TinyInt(_)
        | DataType::SmallInt(_)
        | DataType::MediumInt(_)
        | DataType::Int(_)
        | DataType::Integer(_)
        | DataType::BigInt(_)
        | DataType::Int2(_)
        | DataType::Int4(_)
        | DataType::Int8(_)
        | DataType::Int16
        | DataType::Int32
        | DataType::Int64
        | DataType::Int128
        | DataType::Int256
        // Unsigned forms available in 0.62:
        | DataType::Unsigned
        | DataType::UnsignedInteger => InferredType::Int64,

        // ── Float / decimal family (sqlparser 0.62 variants) ─────────────────
        DataType::Float(_)
        | DataType::FloatUnsigned(_)
        | DataType::Float4
        | DataType::Float8
        | DataType::Float32
        | DataType::Float64
        | DataType::Double(_)
        | DataType::DoublePrecision
        | DataType::Real
        | DataType::Numeric { .. }
        | DataType::Decimal { .. } => InferredType::Float64,

        // ── Everything else → text ───────────────────────────────────────────
        // VARCHAR, TEXT, CHAR, ENUM, DATE, DATETIME, TIMESTAMP, BLOB, etc.
        _ => InferredType::Utf8,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::InferredType;

    fn get_schema(sql: &str) -> Schema {
        extract_schema(sql).unwrap().expect("expected a schema")
    }

    // ── Table name extraction ───────────────────────────────────────────────

    #[test]
    fn simple_table_name() {
        let schema = get_schema("CREATE TABLE users (id INT)");
        assert_eq!(schema.table_name, "users");
    }

    #[test]
    fn backtick_quoted_table_name() {
        // MySQL dumps often backtick-quote names.
        let schema = get_schema("CREATE TABLE `order_items` (id INT)");
        assert!(schema.table_name.contains("order_items"));
    }

    // ── Column names ────────────────────────────────────────────────────────

    #[test]
    fn column_names_extracted() {
        let schema = get_schema("CREATE TABLE t (id INT, name VARCHAR(100), score FLOAT)");
        let names: Vec<&str> = schema.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["id", "name", "score"]);
    }

    #[test]
    fn backtick_column_names_stripped() {
        let schema = get_schema("CREATE TABLE t (`my_col` INT)");
        assert_eq!(schema.columns[0].name, "my_col");
    }

    // ── Type mapping ────────────────────────────────────────────────────────

    #[test]
    fn boolean_type() {
        let schema = get_schema("CREATE TABLE t (flag BOOLEAN)");
        assert_eq!(schema.columns[0].inferred_type, InferredType::Boolean);
    }

    #[test]
    fn integer_types() {
        let schema = get_schema(
            "CREATE TABLE t (a TINYINT, b SMALLINT, c INT, d INTEGER, e BIGINT)",
        );
        for col in &schema.columns {
            assert_eq!(col.inferred_type, InferredType::Int64, "col: {}", col.name);
        }
    }

    #[test]
    fn float_types() {
        let schema = get_schema(
            "CREATE TABLE t (a FLOAT, b DOUBLE, c DECIMAL(10,2), d NUMERIC(8,4))",
        );
        for col in &schema.columns {
            assert_eq!(col.inferred_type, InferredType::Float64, "col: {}", col.name);
        }
    }

    #[test]
    fn text_types_map_to_utf8() {
        let schema = get_schema(
            "CREATE TABLE t (a VARCHAR(255), b TEXT, c CHAR(10))",
        );
        for col in &schema.columns {
            assert_eq!(col.inferred_type, InferredType::Utf8, "col: {}", col.name);
        }
    }

    #[test]
    fn date_time_map_to_utf8() {
        let schema = get_schema("CREATE TABLE t (created_at DATETIME, d DATE)");
        for col in &schema.columns {
            assert_eq!(col.inferred_type, InferredType::Utf8, "col: {}", col.name);
        }
    }

    // ── Non-CREATE-TABLE statements ─────────────────────────────────────────

    #[test]
    fn insert_returns_none() {
        let result = extract_schema("INSERT INTO t (a) VALUES (1)").unwrap();
        assert!(result.is_none(), "INSERT should return None");
    }

    #[test]
    fn set_statement_returns_none() {
        let result = extract_schema("SET NAMES utf8mb4").unwrap();
        assert!(result.is_none());
    }

    // ── Column count ────────────────────────────────────────────────────────

    #[test]
    fn column_count_matches() {
        let schema = get_schema("CREATE TABLE t (a INT, b TEXT, c FLOAT, d BOOLEAN)");
        assert_eq!(schema.column_count(), 4);
    }
}
