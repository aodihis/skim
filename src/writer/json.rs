use std::io::Write;

use crate::parser::{Row, Schema};
use super::{Writer, row_to_json_object};

/// Writes JSON output grouped by table name:
///
/// ```json
/// {
///   "users": [
///     {"id": 1, "name": "Alice"},
///     {"id": 2, "name": "Bob"}
///   ],
///   "orders": [
///     {"id": 1, "total": 99.99}
///   ]
/// }
/// ```
///
/// When the schema's `table_name` changes between `write_row` calls a new
/// group is opened automatically — no need to call `write_header` again.
pub struct JsonWriter<W: Write> {
    out: W,
    started: bool,         // true after the opening `{` has been written
    in_group: bool,        // true while inside a table's `[` array
    first_in_group: bool,  // true for the first row of the current table
    current_table: String, // detect table transitions; non-empty iff ≥1 group opened
}

impl<W: Write> JsonWriter<W> {
    pub fn new(out: W) -> Self {
        Self {
            out,
            started: false,
            in_group: false,
            first_in_group: false,
            current_table: String::new(),
        }
    }
}

impl<W: Write> Writer for JsonWriter<W> {
    /// No-op — the opening object and table groups are started lazily in
    /// `write_row` so we always know the table name.
    fn write_header(&mut self, _schema: &Schema) -> anyhow::Result<()> {
        Ok(())
    }

    fn write_row(&mut self, schema: &Schema, row: &Row) -> anyhow::Result<()> {
        if !self.started {
            self.out.write_all(b"{")?;
            self.started = true;
        }

        if schema.table_name != self.current_table {
            if self.in_group {
                // Close the previous table's array.
                self.out.write_all(b"\n  ]")?;
                self.in_group = false;
            }
            // current_table is non-empty iff at least one group has been opened.
            if !self.current_table.is_empty() {
                self.out.write_all(b",")?;
            }
            let key = serde_json::to_string(&schema.table_name)?;
            write!(self.out, "\n  {key}: [")?;
            self.in_group = true;
            self.current_table = schema.table_name.clone();
            self.first_in_group = true;
        }

        if self.first_in_group {
            self.out.write_all(b"\n    ")?;
            self.first_in_group = false;
        } else {
            self.out.write_all(b",\n    ")?;
        }

        let obj = row_to_json_object(schema, row);
        serde_json::to_writer(&mut self.out, &obj)?;
        Ok(())
    }

    fn finish(&mut self) -> anyhow::Result<()> {
        if self.in_group {
            self.out.write_all(b"\n  ]")?;
        }
        if self.started {
            self.out.write_all(b"\n}\n")?;
        } else {
            self.out.write_all(b"{}\n")?;
        }
        self.out.flush()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{Column, InferredType, Row, Schema, Value};

    fn users_schema() -> Schema {
        Schema {
            table_name: "users".into(),
            columns: vec![
                Column { name: "id".into(),   inferred_type: InferredType::Int64 },
                Column { name: "name".into(), inferred_type: InferredType::Utf8 },
            ],
        }
    }

    fn orders_schema() -> Schema {
        Schema {
            table_name: "orders".into(),
            columns: vec![
                Column { name: "id".into(),    inferred_type: InferredType::Int64 },
                Column { name: "total".into(), inferred_type: InferredType::Float64 },
            ],
        }
    }

    #[test]
    fn single_table_grouped() {
        let schema = users_schema();
        let rows = vec![
            Row { values: vec![Value::Integer(1), Value::Text("Alice".into())] },
            Row { values: vec![Value::Integer(2), Value::Text("Bob".into())] },
        ];
        let mut out = Vec::new();
        let mut w = JsonWriter::new(&mut out);
        for row in &rows {
            w.write_row(&schema, row).unwrap();
        }
        w.finish().unwrap();

        let s = String::from_utf8(out).unwrap();
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert!(v.is_object(), "top level must be an object");
        let arr = v["users"].as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["id"], 1);
        assert_eq!(arr[0]["name"], "Alice");
        assert_eq!(arr[1]["id"], 2);
    }

    #[test]
    fn multiple_tables_grouped() {
        let us = users_schema();
        let os = orders_schema();
        let mut out = Vec::new();
        let mut w = JsonWriter::new(&mut out);
        w.write_row(&us, &Row { values: vec![Value::Integer(1), Value::Text("Alice".into())] }).unwrap();
        w.write_row(&us, &Row { values: vec![Value::Integer(2), Value::Text("Bob".into())] }).unwrap();
        w.write_row(&os, &Row { values: vec![Value::Integer(1), Value::Float(99.99)] }).unwrap();
        w.finish().unwrap();

        let s = String::from_utf8(out).unwrap();
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert!(v.is_object());
        assert_eq!(v["users"].as_array().unwrap().len(), 2);
        assert_eq!(v["orders"].as_array().unwrap().len(), 1);
        assert_eq!(v["orders"][0]["total"], 99.99);
    }

    #[test]
    fn empty_produces_empty_object() {
        let mut out = Vec::new();
        let mut w = JsonWriter::new(&mut out);
        w.finish().unwrap();
        let s = String::from_utf8(out).unwrap();
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert!(v.is_object() && v.as_object().unwrap().is_empty());
    }

    #[test]
    fn write_header_is_noop() {
        let schema = users_schema();
        let mut out = Vec::new();
        let mut w = JsonWriter::new(&mut out);
        // calling write_header multiple times must not produce extra output
        w.write_header(&schema).unwrap();
        w.write_header(&schema).unwrap();
        w.write_row(&schema, &Row { values: vec![Value::Integer(1), Value::Null] }).unwrap();
        w.finish().unwrap();
        let s = String::from_utf8(out).unwrap();
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["users"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn null_value_in_row() {
        let schema = users_schema();
        let mut out = Vec::new();
        let mut w = JsonWriter::new(&mut out);
        w.write_row(&schema, &Row { values: vec![Value::Integer(99), Value::Null] }).unwrap();
        w.finish().unwrap();
        let s = String::from_utf8(out).unwrap();
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["users"][0]["id"], 99);
        assert!(v["users"][0]["name"].is_null());
    }
}
