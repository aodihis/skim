use std::collections::HashSet;
use std::io::Write;

use crate::parser::{Row, Schema, Value};
use super::Writer;

pub struct CsvWriter<W: Write> {
    inner: Option<csv::Writer<W>>,
    no_header: bool,
    current_table: String,    // detect table transitions for blank-line separators
    headers_written: HashSet<String>, // track which tables already have a header
}

impl<W: Write> CsvWriter<W> {
    pub fn new(out: W, no_header: bool) -> Self {
        Self {
            inner: Some(csv::Writer::from_writer(out)),
            no_header,
            current_table: String::new(),
            headers_written: HashSet::new(),
        }
    }

    /// Flush the csv writer, inject a raw `\n` into the underlying writer,
    /// then wrap it in a fresh csv::Writer again.
    fn inject_blank_line(&mut self) -> anyhow::Result<()> {
        let csv_w = self.inner.take().expect("inner must be set");
        match csv_w.into_inner() {
            Ok(mut raw) => {
                raw.write_all(b"\n")?;
                self.inner = Some(csv::Writer::from_writer(raw));
                Ok(())
            }
            Err(e) => {
                // Restore the writer so subsequent calls don't panic on unwrap().
                self.inner = Some(e.into_inner());
                Err(anyhow::anyhow!("CSV flush error"))
            }
        }
    }
}

impl<W: Write> Writer for CsvWriter<W> {
    /// No-op — the header is written lazily on the first `write_row` call for
    /// each table so that multi-table dumps work correctly.
    fn write_header(&mut self, _schema: &Schema) -> anyhow::Result<()> {
        Ok(())
    }

    fn write_row(&mut self, schema: &Schema, row: &Row) -> anyhow::Result<()> {
        if schema.table_name != self.current_table {
            // Separate consecutive different-table blocks with a blank line so
            // the reader can tell where each new table starts.
            if !self.current_table.is_empty() && !self.no_header {
                self.inject_blank_line()?;
            }
            self.current_table = schema.table_name.clone();
            // Write a header only the first time this table is encountered.
            // Non-consecutive appearances of the same table (rare in well-formed
            // dumps) append rows without re-emitting the header.
            if !self.no_header
                && !schema.columns.is_empty()
                && !self.headers_written.contains(&self.current_table)
            {
                let names: Vec<&str> = schema.columns.iter().map(|c| c.name.as_str()).collect();
                self.inner.as_mut().unwrap().write_record(&names)?;
                self.headers_written.insert(self.current_table.clone());
            }
        }
        let record: Vec<String> = row.values.iter().map(value_to_csv).collect();
        self.inner.as_mut().unwrap().write_record(&record)?;
        Ok(())
    }

    fn finish(&mut self) -> anyhow::Result<()> {
        self.inner.as_mut().unwrap().flush()?;
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

    #[test]
    fn multiple_tables_get_separate_headers() {
        let s1 = Schema {
            table_name: "users".into(),
            columns: vec![
                Column { name: "id".into(),   inferred_type: InferredType::Int64 },
                Column { name: "name".into(), inferred_type: InferredType::Utf8 },
            ],
        };
        let s2 = Schema {
            table_name: "orders".into(),
            columns: vec![
                Column { name: "id".into(),    inferred_type: InferredType::Int64 },
                Column { name: "total".into(), inferred_type: InferredType::Float64 },
            ],
        };
        let mut out = Vec::new();
        let mut w = CsvWriter::new(&mut out, false);
        w.write_row(&s1, &Row { values: vec![Value::Integer(1), Value::Text("Alice".into())] }).unwrap();
        w.write_row(&s1, &Row { values: vec![Value::Integer(2), Value::Text("Bob".into())] }).unwrap();
        w.write_row(&s2, &Row { values: vec![Value::Integer(1), Value::Float(99.99)] }).unwrap();
        w.finish().unwrap();
        drop(w);

        let s = String::from_utf8(out).unwrap();
        let sections: Vec<&str> = s.split("\n\n").collect();
        assert_eq!(sections.len(), 2, "two tables should produce two CSV sections");

        let mut rdr1 = csv::Reader::from_reader(sections[0].as_bytes());
        assert_eq!(rdr1.headers().unwrap().iter().collect::<Vec<_>>(), ["id", "name"]);
        assert_eq!(rdr1.records().count(), 2);

        let mut rdr2 = csv::Reader::from_reader(sections[1].as_bytes());
        assert_eq!(rdr2.headers().unwrap().iter().collect::<Vec<_>>(), ["id", "total"]);
        assert_eq!(rdr2.records().count(), 1);
    }

    #[test]
    fn nonconsecutive_same_table_no_duplicate_header() {
        let s1 = Schema {
            table_name: "users".into(),
            columns: vec![Column { name: "id".into(), inferred_type: InferredType::Int64 }],
        };
        let s2 = Schema {
            table_name: "orders".into(),
            columns: vec![Column { name: "total".into(), inferred_type: InferredType::Float64 }],
        };
        let mut out = Vec::new();
        let mut w = CsvWriter::new(&mut out, false);
        w.write_row(&s1, &Row { values: vec![Value::Integer(1)] }).unwrap();
        w.write_row(&s2, &Row { values: vec![Value::Float(9.99)] }).unwrap();
        // users appears again — must NOT emit a second "id" header
        w.write_row(&s1, &Row { values: vec![Value::Integer(2)] }).unwrap();
        w.finish().unwrap();
        drop(w);

        let s = String::from_utf8(out).unwrap();
        // Count occurrences of "id" header — should appear exactly once
        assert_eq!(s.lines().filter(|l| *l == "id").count(), 1,
            "users header should appear only once:\n{s}");
    }
}
