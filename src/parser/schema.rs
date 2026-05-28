//! Extract a `Schema` from a `CREATE TABLE` SQL statement.
//!
//! # References
//! - CreateTable AST: <https://docs.rs/sqlparser/latest/sqlparser/ast/struct.CreateTable.html>
//! - DataType enum:   <https://docs.rs/sqlparser/latest/sqlparser/ast/enum.DataType.html>

use sqlparser::ast::{DataType, ObjectName, Statement};
use sqlparser::dialect::{MySqlDialect, PostgreSqlDialect};
use sqlparser::parser::Parser;

use super::{Column, InferredType, Schema, SqlDialect};

// ── Public API ────────────────────────────────────────────────────────────────

/// Try to extract a `Schema` from a SQL statement string.
///
/// Returns `Some(Schema)` if the statement is a `CREATE TABLE`.
/// Returns `None` for any other statement (INSERT, SET, etc.).
///
/// For PostgreSQL, schema-qualified names like `public.users` are stored as
/// just `users` (the unqualified part) so that `-t users` matches both dialects.
pub fn extract_schema(sql: &str, dialect: SqlDialect) -> anyhow::Result<Option<Schema>> {
    let stmts = match dialect {
        SqlDialect::Mysql    => Parser::parse_sql(&MySqlDialect {},    sql)?,
        SqlDialect::Postgres => Parser::parse_sql(&PostgreSqlDialect {}, sql)?,
    };

    let Some(stmt) = stmts.into_iter().next() else {
        return Ok(None);
    };

    let Statement::CreateTable(ct) = stmt else {
        return Ok(None);
    };

    let table_name = unqualified_name(&ct.name);

    let columns = ct
        .columns
        .iter()
        .map(|col_def| Column {
            name: col_def.name.value.clone(),
            inferred_type: data_type_to_inferred(&col_def.data_type),
        })
        .collect();

    Ok(Some(Schema { table_name, columns }))
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Return just the last identifier component of a (possibly schema-qualified) name.
/// `public.users` → `users`,  `"public"."users"` → `users`,  `users` → `users`.
pub fn unqualified_name(name: &ObjectName) -> String {
    // ObjectName::to_string() produces "schema.table" or `schema`.`table` etc.
    // Split on '.' and take the last segment, then strip any wrapping quotes.
    let full = name.to_string();
    full.split('.')
        .next_back()
        .unwrap_or(&full)
        .trim_matches('"')
        .trim_matches('`')
        .to_string()
}

// ── DataType → InferredType ───────────────────────────────────────────────────

/// Map a sqlparser `DataType` to our `InferredType`.
fn data_type_to_inferred(dt: &DataType) -> InferredType {
    match dt {
        // ── Boolean ──────────────────────────────────────────────────────────
        DataType::Boolean | DataType::Bool => InferredType::Boolean,

        // ── Integer family ───────────────────────────────────────────────────
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
        | DataType::Unsigned
        | DataType::UnsignedInteger => InferredType::Int64,

        // ── Float / decimal family ────────────────────────────────────────────
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

        // ── PostgreSQL custom types ───────────────────────────────────────────
        // sqlparser parses SERIAL / SMALLSERIAL / BIGSERIAL as Custom types.
        DataType::Custom(name, _) => {
            match name.to_string().to_lowercase().as_str() {
                "serial" | "smallserial" | "bigserial" => InferredType::Int64,
                _ => InferredType::Utf8,
            }
        }

        // ── Everything else → text ───────────────────────────────────────────
        // VARCHAR, TEXT, CHAR, ENUM, DATE, DATETIME, TIMESTAMP, BLOB,
        // UUID, JSON, JSONB, CITEXT, etc.
        _ => InferredType::Utf8,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::InferredType;

    fn mysql_schema(sql: &str) -> Schema {
        extract_schema(sql, SqlDialect::Mysql).unwrap().expect("expected a schema")
    }

    fn pg_schema(sql: &str) -> Schema {
        extract_schema(sql, SqlDialect::Postgres).unwrap().expect("expected a schema")
    }

    // ── MySQL: table name extraction ────────────────────────────────────────

    #[test]
    fn simple_table_name() {
        let schema = mysql_schema("CREATE TABLE users (id INT)");
        assert_eq!(schema.table_name, "users");
    }

    #[test]
    fn backtick_quoted_table_name() {
        let schema = mysql_schema("CREATE TABLE `order_items` (id INT)");
        assert_eq!(schema.table_name, "order_items");
    }

    // ── MySQL: column names ─────────────────────────────────────────────────

    #[test]
    fn column_names_extracted() {
        let schema = mysql_schema("CREATE TABLE t (id INT, name VARCHAR(100), score FLOAT)");
        let names: Vec<&str> = schema.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["id", "name", "score"]);
    }

    #[test]
    fn backtick_column_names_stripped() {
        let schema = mysql_schema("CREATE TABLE t (`my_col` INT)");
        assert_eq!(schema.columns[0].name, "my_col");
    }

    // ── MySQL: type mapping ─────────────────────────────────────────────────

    #[test]
    fn boolean_type() {
        let schema = mysql_schema("CREATE TABLE t (flag BOOLEAN)");
        assert_eq!(schema.columns[0].inferred_type, InferredType::Boolean);
    }

    #[test]
    fn integer_types() {
        let schema = mysql_schema(
            "CREATE TABLE t (a TINYINT, b SMALLINT, c INT, d INTEGER, e BIGINT)",
        );
        for col in &schema.columns {
            assert_eq!(col.inferred_type, InferredType::Int64, "col: {}", col.name);
        }
    }

    #[test]
    fn float_types() {
        let schema = mysql_schema(
            "CREATE TABLE t (a FLOAT, b DOUBLE, c DECIMAL(10,2), d NUMERIC(8,4))",
        );
        for col in &schema.columns {
            assert_eq!(col.inferred_type, InferredType::Float64, "col: {}", col.name);
        }
    }

    #[test]
    fn text_types_map_to_utf8() {
        let schema = mysql_schema(
            "CREATE TABLE t (a VARCHAR(255), b TEXT, c CHAR(10))",
        );
        for col in &schema.columns {
            assert_eq!(col.inferred_type, InferredType::Utf8, "col: {}", col.name);
        }
    }

    #[test]
    fn date_time_map_to_utf8() {
        let schema = mysql_schema("CREATE TABLE t (created_at DATETIME, d DATE)");
        for col in &schema.columns {
            assert_eq!(col.inferred_type, InferredType::Utf8, "col: {}", col.name);
        }
    }

    // ── Non-CREATE-TABLE statements ─────────────────────────────────────────

    #[test]
    fn insert_returns_none() {
        let result = extract_schema("INSERT INTO t (a) VALUES (1)", SqlDialect::Mysql).unwrap();
        assert!(result.is_none(), "INSERT should return None");
    }

    #[test]
    fn set_statement_returns_none() {
        let result = extract_schema("SET NAMES utf8mb4", SqlDialect::Mysql).unwrap();
        assert!(result.is_none());
    }

    // ── Column count ────────────────────────────────────────────────────────

    #[test]
    fn column_count_matches() {
        let schema = mysql_schema("CREATE TABLE t (a INT, b TEXT, c FLOAT, d BOOLEAN)");
        assert_eq!(schema.column_count(), 4);
    }

    // ── PostgreSQL: schema-qualified table names ────────────────────────────

    #[test]
    fn pg_schema_qualified_table_name_stripped() {
        // pg_dump writes CREATE TABLE public.users — we store just "users".
        let schema = pg_schema("CREATE TABLE public.users (id INTEGER)");
        assert_eq!(schema.table_name, "users");
    }

    #[test]
    fn pg_double_quoted_table_name() {
        let schema = pg_schema(r#"CREATE TABLE "users" (id INTEGER)"#);
        assert_eq!(schema.table_name, "users");
    }

    #[test]
    fn pg_double_quoted_column_names() {
        let schema = pg_schema(r#"CREATE TABLE t ("user_id" INTEGER, "full_name" TEXT)"#);
        assert_eq!(schema.columns[0].name, "user_id");
        assert_eq!(schema.columns[1].name, "full_name");
    }

    // ── PostgreSQL: type mappings ───────────────────────────────────────────

    #[test]
    fn pg_serial_maps_to_int64() {
        let schema = pg_schema(
            "CREATE TABLE t (id SERIAL, small SMALLSERIAL, big BIGSERIAL)",
        );
        for col in &schema.columns {
            assert_eq!(col.inferred_type, InferredType::Int64, "col: {}", col.name);
        }
    }

    #[test]
    fn pg_text_maps_to_utf8() {
        let schema = pg_schema("CREATE TABLE t (a TEXT, b VARCHAR(100), c CHAR(10))");
        for col in &schema.columns {
            assert_eq!(col.inferred_type, InferredType::Utf8, "col: {}", col.name);
        }
    }

    #[test]
    fn pg_boolean_maps_to_boolean() {
        let schema = pg_schema("CREATE TABLE t (active BOOLEAN)");
        assert_eq!(schema.columns[0].inferred_type, InferredType::Boolean);
    }

    #[test]
    fn pg_uuid_and_json_map_to_utf8() {
        let schema = pg_schema("CREATE TABLE t (id UUID, meta JSONB, doc JSON)");
        for col in &schema.columns {
            assert_eq!(col.inferred_type, InferredType::Utf8, "col: {}", col.name);
        }
    }

    #[test]
    fn pg_numeric_maps_to_float64() {
        let schema = pg_schema("CREATE TABLE t (price NUMERIC(10,2), rate DECIMAL(8,4))");
        for col in &schema.columns {
            assert_eq!(col.inferred_type, InferredType::Float64, "col: {}", col.name);
        }
    }

    #[test]
    fn pg_timestamp_maps_to_utf8() {
        let schema = pg_schema(
            "CREATE TABLE t (created_at TIMESTAMP, updated_at TIMESTAMPTZ)",
        );
        for col in &schema.columns {
            assert_eq!(col.inferred_type, InferredType::Utf8, "col: {}", col.name);
        }
    }
}
