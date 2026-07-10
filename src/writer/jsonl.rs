use std::io::Write;

use super::{row_to_json_object, Writer};
use crate::parser::{Row, Schema};

pub struct JsonlWriter<W: Write> {
    out: W,
}

impl<W: Write> JsonlWriter<W> {
    pub fn new(out: W) -> Self {
        Self { out }
    }
}

impl<W: Write> Writer for JsonlWriter<W> {
    fn write_header(&mut self, _schema: &Schema) -> anyhow::Result<()> {
        Ok(())
    }

    fn write_row(&mut self, schema: &Schema, row: &Row) -> anyhow::Result<()> {
        let obj = row_to_json_object(schema, row);
        serde_json::to_writer(&mut self.out, &obj)?;
        self.out.write_all(b"\n")?;
        Ok(())
    }

    fn finish(&mut self) -> anyhow::Result<()> {
        self.out.flush()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{Column, InferredType, Row, Schema, Value};

    fn schema() -> Schema {
        Schema {
            table_name: "t".into(),
            columns: vec![
                Column {
                    name: "id".into(),
                    inferred_type: InferredType::Int64,
                },
                Column {
                    name: "name".into(),
                    inferred_type: InferredType::Utf8,
                },
                Column {
                    name: "active".into(),
                    inferred_type: InferredType::Boolean,
                },
            ],
        }
    }

    #[test]
    fn one_object_per_line() {
        let schema = schema();
        let mut out = Vec::new();
        let mut w = JsonlWriter::new(&mut out);
        w.write_header(&schema).unwrap();
        w.write_row(
            &schema,
            &Row {
                values: vec![
                    Value::Integer(1),
                    Value::Text("Alice".into()),
                    Value::Bool(true),
                ],
            },
        )
        .unwrap();
        w.write_row(
            &schema,
            &Row {
                values: vec![Value::Integer(2), Value::Text("Bob".into()), Value::Null],
            },
        )
        .unwrap();
        w.finish().unwrap();

        let s = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = s.lines().collect();
        assert_eq!(lines.len(), 2);

        let obj1: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(obj1["id"], 1);
        assert_eq!(obj1["name"], "Alice");
        assert_eq!(obj1["active"], true);

        let obj2: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(obj2["id"], 2);
        assert!(obj2["active"].is_null());
    }

    #[test]
    fn null_serialises_as_json_null() {
        let schema = schema();
        let mut out = Vec::new();
        let mut w = JsonlWriter::new(&mut out);
        w.write_header(&schema).unwrap();
        w.write_row(
            &schema,
            &Row {
                values: vec![Value::Null, Value::Null, Value::Null],
            },
        )
        .unwrap();
        w.finish().unwrap();

        let s = String::from_utf8(out).unwrap();
        let obj: serde_json::Value = serde_json::from_str(s.trim()).unwrap();
        assert!(obj["id"].is_null());
        assert!(obj["name"].is_null());
        assert!(obj["active"].is_null());
    }

    #[test]
    fn float_value() {
        let schema = Schema {
            table_name: "t".into(),
            columns: vec![Column {
                name: "score".into(),
                inferred_type: InferredType::Float64,
            }],
        };
        let mut out = Vec::new();
        let mut w = JsonlWriter::new(&mut out);
        w.write_header(&schema).unwrap();
        w.write_row(
            &schema,
            &Row {
                values: vec![Value::Float(3.14)],
            },
        )
        .unwrap();
        w.finish().unwrap();

        let s = String::from_utf8(out).unwrap();
        let obj: serde_json::Value = serde_json::from_str(s.trim()).unwrap();
        assert!((obj["score"].as_f64().unwrap() - 3.14).abs() < 1e-10);
    }
}
