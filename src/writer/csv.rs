use std::io::Write;

use crate::parser::{Row, Schema, Value};
use super::Writer;

pub struct CsvWriter<W: Write> {
    inner: csv::Writer<W>,
    no_header: bool,
    header_written: bool,
}

impl<W: Write> CsvWriter<W> {
    pub fn new(out: W, no_header: bool) -> Self {
        Self {
            inner: csv::Writer::from_writer(out),
            no_header,
            header_written: false,
        }
    }
}

impl<W: Write> Writer for CsvWriter<W> {
    fn write_header(&mut self, schema: &Schema) -> anyhow::Result<()> {
        if !self.no_header && !self.header_written && !schema.columns.is_empty() {
            let names: Vec<&str> = schema.columns.iter().map(|c| c.name.as_str()).collect();
            self.inner.write_record(&names)?;
            self.header_written = true;
        }
        Ok(())
    }

    fn write_row(&mut self, schema: &Schema, row: &Row) -> anyhow::Result<()> {
        // If header not yet written (schema was empty at write_header time), write it now.
        if !self.no_header && !self.header_written && !schema.columns.is_empty() {
            let names: Vec<&str> = schema.columns.iter().map(|c| c.name.as_str()).collect();
            self.inner.write_record(&names)?;
            self.header_written = true;
        }
        let record: Vec<String> = row.values.iter().map(value_to_csv).collect();
        self.inner.write_record(&record)?;
        Ok(())
    }

    fn finish(&mut self) -> anyhow::Result<()> {
        self.inner.flush()?;
        Ok(())
    }
}

fn value_to_csv(v: &Value) -> String {
    match v {
        Value::Null       => String::new(),
        Value::Bool(b)    => b.to_string(),
        Value::Integer(n) => n.to_string(),
        Value::Float(f)   => f.to_string(),
        Value::Text(s)    => s.clone(),
        Value::Bytes(b)   => b.iter().map(|x| format!("{x:02x}")).collect(),
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
                Column { name: "id".into(),    inferred_type: InferredType::Int64 },
                Column { name: "name".into(),  inferred_type: InferredType::Utf8 },
                Column { name: "score".into(), inferred_type: InferredType::Float64 },
            ],
        }
    }

    #[test]
    fn header_and_rows() {
        let schema = schema();
        let mut out = Vec::new();
        let mut w = CsvWriter::new(&mut out, false);
        w.write_header(&schema).unwrap();
        w.write_row(&schema, &Row { values: vec![
            Value::Integer(1), Value::Text("Alice".into()), Value::Float(9.5),
        ]}).unwrap();
        w.write_row(&schema, &Row { values: vec![
            Value::Integer(2), Value::Text("Bob".into()), Value::Null,
        ]}).unwrap();
        w.finish().unwrap();
        drop(w);

        let s = String::from_utf8(out).unwrap();
        let mut rdr = csv::Reader::from_reader(s.as_bytes());

        let hdrs: Vec<&str> = rdr.headers().unwrap().iter().collect();
        assert_eq!(hdrs, ["id", "name", "score"]);

        let records: Vec<csv::StringRecord> = rdr.records()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(records.len(), 2);
        assert_eq!(&records[0][0], "1");
        assert_eq!(&records[0][1], "Alice");
        assert_eq!(&records[1][2], "");  // NULL → empty
    }

    #[test]
    fn no_header_flag() {
        let schema = schema();
        let mut out = Vec::new();
        let mut w = CsvWriter::new(&mut out, true); // no_header = true
        w.write_header(&schema).unwrap();
        w.write_row(&schema, &Row { values: vec![
            Value::Integer(1), Value::Text("A".into()), Value::Float(1.0),
        ]}).unwrap();
        w.finish().unwrap();
        drop(w);

        let s = String::from_utf8(out).unwrap();
        // With no_header, reading without headers gives us the first row directly.
        let mut rdr = csv::ReaderBuilder::new().has_headers(false).from_reader(s.as_bytes());
        let records: Vec<csv::StringRecord> = rdr.records().map(|r| r.unwrap()).collect();
        assert_eq!(records.len(), 1);
        assert_eq!(&records[0][0], "1");
    }

    #[test]
    fn string_with_comma_quoted() {
        let schema = Schema {
            table_name: "t".into(),
            columns: vec![Column { name: "msg".into(), inferred_type: InferredType::Utf8 }],
        };
        let mut out = Vec::new();
        let mut w = CsvWriter::new(&mut out, false);
        w.write_header(&schema).unwrap();
        w.write_row(&schema, &Row { values: vec![
            Value::Text("hello, world".into()),
        ]}).unwrap();
        w.finish().unwrap();
        drop(w);

        let s = String::from_utf8(out).unwrap();
        let mut rdr = csv::Reader::from_reader(s.as_bytes());
        let records: Vec<csv::StringRecord> = rdr.records().map(|r| r.unwrap()).collect();
        assert_eq!(&records[0][0], "hello, world");
    }
}
