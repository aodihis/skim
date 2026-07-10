use std::io::Write;

use super::Writer;
use crate::parser::{Row, Schema, Value};

pub struct YamlWriter<W: Write> {
    out: W,
}

impl<W: Write> YamlWriter<W> {
    pub fn new(out: W) -> Self {
        Self { out }
    }
}

impl<W: Write> Writer for YamlWriter<W> {
    fn write_header(&mut self, _schema: &Schema) -> anyhow::Result<()> {
        Ok(())
    }

    fn write_row(&mut self, schema: &Schema, row: &Row) -> anyhow::Result<()> {
        let mut mapping = serde_yaml::Mapping::new();
        for (col, val) in schema.columns.iter().zip(row.values.iter()) {
            mapping.insert(
                serde_yaml::Value::String(col.name.clone()),
                value_to_yaml(val),
            );
        }
        let doc = serde_yaml::Value::Mapping(mapping);
        let yaml = serde_yaml::to_string(&doc)?;
        // serde_yaml may or may not include a leading "---\n"; normalise so each
        // document always starts with exactly one "---" separator.
        let body = yaml.strip_prefix("---\n").unwrap_or(&yaml);
        write!(self.out, "---\n{}", body)?;
        Ok(())
    }

    fn finish(&mut self) -> anyhow::Result<()> {
        self.out.flush()?;
        Ok(())
    }
}

fn value_to_yaml(v: &Value) -> serde_yaml::Value {
    match v {
        Value::Null => serde_yaml::Value::Null,
        Value::Bool(b) => serde_yaml::Value::Bool(*b),
        Value::Integer(n) => serde_yaml::Value::Number((*n).into()),
        Value::Float(f) => serde_yaml::Value::Number((*f).into()),
        Value::Text(s) => serde_yaml::Value::String(s.clone()),
        Value::Bytes(b) => {
            serde_yaml::Value::String(b.iter().map(|x| format!("{x:02x}")).collect())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{Column, InferredType, Row, Schema, Value};
    use serde::de::Deserialize;

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
            ],
        }
    }

    #[test]
    fn produces_multi_document_yaml() {
        let schema = schema();
        let mut out = Vec::new();
        let mut w = YamlWriter::new(&mut out);
        w.write_header(&schema).unwrap();
        w.write_row(
            &schema,
            &Row {
                values: vec![Value::Integer(1), Value::Text("Alice".into())],
            },
        )
        .unwrap();
        w.write_row(
            &schema,
            &Row {
                values: vec![Value::Integer(2), Value::Text("Bob".into())],
            },
        )
        .unwrap();
        w.finish().unwrap();

        let s = String::from_utf8(out).unwrap();
        // Parse as YAML multi-document stream.
        let docs: Vec<serde_yaml::Value> = serde_yaml::Deserializer::from_str(&s)
            .map(|de| serde_yaml::Value::deserialize(de).unwrap())
            .collect();
        assert_eq!(docs.len(), 2);
        assert_eq!(docs[0]["id"].as_i64(), Some(1));
        assert_eq!(docs[0]["name"].as_str(), Some("Alice"));
        assert_eq!(docs[1]["id"].as_i64(), Some(2));
    }

    #[test]
    fn null_serialises_as_yaml_null() {
        let schema = schema();
        let mut out = Vec::new();
        let mut w = YamlWriter::new(&mut out);
        w.write_header(&schema).unwrap();
        w.write_row(
            &schema,
            &Row {
                values: vec![Value::Null, Value::Null],
            },
        )
        .unwrap();
        w.finish().unwrap();

        let s = String::from_utf8(out).unwrap();
        let docs: Vec<serde_yaml::Value> = serde_yaml::Deserializer::from_str(&s)
            .map(|de| serde_yaml::Value::deserialize(de).unwrap())
            .collect();
        assert_eq!(docs.len(), 1);
        assert!(docs[0]["id"].is_null());
        assert!(docs[0]["name"].is_null());
    }

    #[test]
    fn boolean_values() {
        let schema = Schema {
            table_name: "t".into(),
            columns: vec![Column {
                name: "flag".into(),
                inferred_type: InferredType::Boolean,
            }],
        };
        let mut out = Vec::new();
        let mut w = YamlWriter::new(&mut out);
        w.write_header(&schema).unwrap();
        w.write_row(
            &schema,
            &Row {
                values: vec![Value::Bool(true)],
            },
        )
        .unwrap();
        w.finish().unwrap();

        let s = String::from_utf8(out).unwrap();
        let docs: Vec<serde_yaml::Value> = serde_yaml::Deserializer::from_str(&s)
            .map(|de| serde_yaml::Value::deserialize(de).unwrap())
            .collect();
        assert_eq!(docs[0]["flag"].as_bool(), Some(true));
    }
}
