pub mod csv;
pub mod json;
pub mod jsonl;
pub mod parquet;
pub mod toml;
pub mod yaml;

use crate::parser::{Row, Schema, Value};

/// Common interface for all output format writers.
pub trait Writer {
    /// Called once before any rows, with the final resolved schema.
    fn write_header(&mut self, schema: &Schema) -> anyhow::Result<()>;

    /// Called for each row. `schema` is passed again for column name lookups.
    fn write_row(&mut self, schema: &Schema, row: &Row) -> anyhow::Result<()>;

    /// Called once at the end. Must flush all buffers and finalise the output.
    fn finish(&mut self) -> anyhow::Result<()>;
}

// ── Shared JSON helpers (used by both json.rs and jsonl.rs) ──────────────────

pub(super) fn row_to_json_object(
    schema: &Schema,
    row: &Row,
) -> serde_json::Map<String, serde_json::Value> {
    let mut map = serde_json::Map::new();
    for (col, val) in schema.columns.iter().zip(row.values.iter()) {
        map.insert(col.name.clone(), value_to_json(val));
    }
    map
}

pub(super) fn value_to_json(v: &Value) -> serde_json::Value {
    match v {
        Value::Null       => serde_json::Value::Null,
        Value::Bool(b)    => serde_json::Value::Bool(*b),
        Value::Integer(n) => serde_json::Value::Number((*n).into()),
        Value::Float(f)   => serde_json::Number::from_f64(*f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        Value::Text(s)    => serde_json::Value::String(s.clone()),
        Value::Bytes(b)   => serde_json::Value::String(
            b.iter().map(|x| format!("{x:02x}")).collect(),
        ),
    }
}
