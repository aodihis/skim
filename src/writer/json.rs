use std::io::Write;

use crate::parser::{Row, Schema};
use super::{Writer, row_to_json_object};

pub struct JsonWriter<W: Write> {
    out: W,
    first: bool,
    header_written: bool,
}

impl<W: Write> JsonWriter<W> {
    pub fn new(out: W) -> Self {
        Self { out, first: true, header_written: false }
    }
}

impl<W: Write> Writer for JsonWriter<W> {
    fn write_header(&mut self, _schema: &Schema) -> anyhow::Result<()> {
        if self.header_written {
            return Ok(());
        }
        self.header_written = true;
        self.out.write_all(b"[\n")?;
        Ok(())
    }

    fn write_row(&mut self, schema: &Schema, row: &Row) -> anyhow::Result<()> {
        if !self.header_written {
            self.write_header(schema)?;
        }
        if !self.first {
            self.out.write_all(b",\n")?;
        }
        self.first = false;
        let obj = row_to_json_object(schema, row);
        serde_json::to_writer(&mut self.out, &obj)?;
        Ok(())
    }

    fn finish(&mut self) -> anyhow::Result<()> {
        if self.header_written {
            self.out.write_all(b"\n]\n")?;
        } else {
            // finish() called without write_header() — emit a valid empty array.
            self.out.write_all(b"[]\n")?;
        }
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
                Column { name: "id".into(),   inferred_type: InferredType::Int64 },
                Column { name: "name".into(), inferred_type: InferredType::Utf8 },
            ],
        }
    }

    fn write_rows(rows: &[Row]) -> String {
        let schema = schema();
        let mut out = Vec::new();
        let mut w = JsonWriter::new(&mut out);
        w.write_header(&schema).unwrap();
        for row in rows {
            w.write_row(&schema, row).unwrap();
        }
        w.finish().unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn produces_valid_json_array() {
        let rows = vec![
            Row { values: vec![Value::Integer(1), Value::Text("Alice".into())] },
            Row { values: vec![Value::Integer(2), Value::Text("Bob".into())] },
        ];
        let s = write_rows(&rows);
        let arr: Vec<serde_json::Value> = serde_json::from_str(&s).unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["id"], 1);
        assert_eq!(arr[0]["name"], "Alice");
        assert_eq!(arr[1]["id"], 2);
    }

    #[test]
    fn empty_produces_valid_empty_array() {
        let s = write_rows(&[]);
        let arr: Vec<serde_json::Value> = serde_json::from_str(&s).unwrap();
        assert!(arr.is_empty());
    }

    #[test]
    fn finish_without_write_header_produces_empty_array() {
        let mut out = Vec::new();
        let mut w = JsonWriter::new(&mut out);
        w.finish().unwrap();
        let s = String::from_utf8(out).unwrap();
        let arr: Vec<serde_json::Value> = serde_json::from_str(&s).unwrap();
        assert!(arr.is_empty());
    }

    #[test]
    fn double_write_header_is_idempotent() {
        let schema = schema();
        let mut out = Vec::new();
        let mut w = JsonWriter::new(&mut out);
        w.write_header(&schema).unwrap();
        w.write_header(&schema).unwrap(); // second call must be a no-op
        w.write_row(&schema, &Row { values: vec![Value::Integer(1), Value::Text("x".into())] }).unwrap();
        w.finish().unwrap();
        let s = String::from_utf8(out).unwrap();
        let arr: Vec<serde_json::Value> = serde_json::from_str(&s).unwrap();
        assert_eq!(arr.len(), 1);
    }

    #[test]
    fn single_row_no_trailing_comma() {
        let rows = vec![Row { values: vec![Value::Integer(99), Value::Null] }];
        let s = write_rows(&rows);
        let arr: Vec<serde_json::Value> = serde_json::from_str(&s).unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["id"], 99);
        assert!(arr[0]["name"].is_null());
    }
}
