use std::io::Write;

use crate::parser::{Row, Schema, Value};
use super::Writer;

pub struct TomlWriter<W: Write> {
    out: W,
    array_key: String,
}

impl<W: Write> TomlWriter<W> {
    pub fn new(out: W) -> Self {
        Self { out, array_key: "rows".to_string() }
    }
}

impl<W: Write> Writer for TomlWriter<W> {
    fn write_header(&mut self, schema: &Schema) -> anyhow::Result<()> {
        if !schema.table_name.is_empty() {
            self.array_key = sanitize_key(&schema.table_name);
        }
        Ok(())
    }

    fn write_row(&mut self, schema: &Schema, row: &Row) -> anyhow::Result<()> {
        // Update key lazily in case write_header saw an empty schema.
        if self.array_key == "rows" && !schema.table_name.is_empty() {
            self.array_key = sanitize_key(&schema.table_name);
        }

        let mut map = toml::map::Map::new();
        for (col, val) in schema.columns.iter().zip(row.values.iter()) {
            if let Some(tv) = value_to_toml(val) {
                map.insert(col.name.clone(), tv);
            }
            // NULL: TOML has no null — skip the key entirely.
        }

        let serialized = toml::to_string(&toml::Value::Table(map))?;
        write!(self.out, "[[{key}]]\n{serialized}\n", key = self.array_key)?;
        Ok(())
    }

    fn finish(&mut self) -> anyhow::Result<()> {
        self.out.flush()?;
        Ok(())
    }
}

fn value_to_toml(v: &Value) -> Option<toml::Value> {
    match v {
        Value::Null       => None,
        Value::Bool(b)    => Some(toml::Value::Boolean(*b)),
        Value::Integer(n) => Some(toml::Value::Integer(*n)),
        Value::Float(f)   => {
            if f.is_nan() || f.is_infinite() {
                // TOML spec does not allow NaN/Inf — represent as string.
                Some(toml::Value::String(f.to_string()))
            } else {
                Some(toml::Value::Float(*f))
            }
        }
        Value::Text(s)    => Some(toml::Value::String(s.clone())),
        Value::Bytes(b)   => Some(toml::Value::String(
            b.iter().map(|x| format!("{x:02x}")).collect(),
        )),
    }
}

/// Make a valid TOML bare key: strip wrapping backticks/quotes, replace
/// non-alphanumeric/hyphen/underscore characters with underscores.
fn sanitize_key(name: &str) -> String {
    let stripped = name.trim_matches('`').trim_matches('"').trim_matches('\'');
    let mut key: String = stripped
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    if key.is_empty() {
        key = "rows".to_string();
    }
    key
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{Column, InferredType, Row, Schema, Value};

    fn schema() -> Schema {
        Schema {
            table_name: "users".into(),
            columns: vec![
                Column { name: "id".into(),   inferred_type: InferredType::Int64 },
                Column { name: "name".into(), inferred_type: InferredType::Utf8 },
                Column { name: "score".into(), inferred_type: InferredType::Float64 },
            ],
        }
    }

    #[test]
    fn array_of_tables() {
        let mut out = Vec::new();
        let mut w = TomlWriter::new(&mut out);
        w.write_header(&schema()).unwrap();
        w.write_row(&schema(), &Row { values: vec![
            Value::Integer(1), Value::Text("Alice".into()), Value::Float(9.5),
        ]}).unwrap();
        w.write_row(&schema(), &Row { values: vec![
            Value::Integer(2), Value::Text("Bob".into()), Value::Float(7.0),
        ]}).unwrap();
        w.finish().unwrap();
        drop(w);

        let s = String::from_utf8(out).unwrap();
        let parsed: toml::Value = toml::from_str(&s).unwrap();
        let arr = parsed.get("users").unwrap().as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0].get("id").unwrap().as_integer().unwrap(), 1);
        assert_eq!(arr[0].get("name").unwrap().as_str().unwrap(), "Alice");
        assert_eq!(arr[1].get("id").unwrap().as_integer().unwrap(), 2);
    }

    #[test]
    fn null_skips_key() {
        let schema = Schema {
            table_name: "t".into(),
            columns: vec![
                Column { name: "a".into(), inferred_type: InferredType::Int64 },
                Column { name: "b".into(), inferred_type: InferredType::Utf8 },
            ],
        };
        let mut out = Vec::new();
        let mut w = TomlWriter::new(&mut out);
        w.write_header(&schema).unwrap();
        w.write_row(&schema, &Row { values: vec![Value::Integer(1), Value::Null] }).unwrap();
        w.finish().unwrap();
        drop(w);

        let s = String::from_utf8(out).unwrap();
        let parsed: toml::Value = toml::from_str(&s).unwrap();
        let row = &parsed.get("t").unwrap().as_array().unwrap()[0];
        assert!(row.get("a").is_some());
        assert!(row.get("b").is_none(), "NULL should be omitted");
    }

    #[test]
    fn nan_becomes_string() {
        let schema = Schema {
            table_name: "t".into(),
            columns: vec![Column { name: "x".into(), inferred_type: InferredType::Float64 }],
        };
        let mut out = Vec::new();
        let mut w = TomlWriter::new(&mut out);
        w.write_header(&schema).unwrap();
        w.write_row(&schema, &Row { values: vec![Value::Float(f64::NAN)] }).unwrap();
        w.finish().unwrap();
        drop(w);

        let s = String::from_utf8(out).unwrap();
        let parsed: toml::Value = toml::from_str(&s).unwrap();
        let row = &parsed.get("t").unwrap().as_array().unwrap()[0];
        // NaN is stored as a string because TOML spec doesn't allow it.
        assert!(row.get("x").unwrap().is_str());
    }
}
