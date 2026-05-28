use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow_array::builder::{BooleanBuilder, Float64Builder, Int64Builder, StringBuilder};
use arrow_array::{ArrayRef, RecordBatch};
use arrow_schema::{DataType, Field, Schema as ArrowSchema};
use parquet::arrow::ArrowWriter;
use parquet::file::properties::WriterProperties;

use crate::parser::{InferredType, Row, Schema, Value};
use super::Writer;

// ── Public writer ─────────────────────────────────────────────────────────────

pub struct ParquetWriter {
    path: PathBuf,
    batch_size: usize,
    infer_rows: usize,

    /// SQL schema from CREATE TABLE (may have empty columns if no DDL was seen).
    sql_schema: Schema,
    /// Per-column inferred types — widened as rows arrive during inference phase.
    col_types: Vec<InferredType>,
    /// Rows buffered either for inference (pre-schema) or for batch flushing.
    pending: Vec<Row>,

    /// Set once the Arrow schema is resolved (either from CREATE TABLE or inference).
    arrow_schema: Option<Arc<ArrowSchema>>,
    /// Set at the same time as arrow_schema.
    writer: Option<ArrowWriter<File>>,
}

impl ParquetWriter {
    pub fn new(path: &Path, batch_size: usize, infer_rows: usize) -> anyhow::Result<Self> {
        Ok(Self {
            path: path.to_path_buf(),
            batch_size,
            infer_rows,
            sql_schema: Schema { table_name: String::new(), columns: vec![] },
            col_types: vec![],
            pending: vec![],
            arrow_schema: None,
            writer: None,
        })
    }

    /// True when every column has a concrete type (no Unknown left).
    fn all_types_known(&self) -> bool {
        !self.col_types.is_empty()
            && self.col_types.iter().all(|t| *t != InferredType::Unknown)
    }

    /// Resolve Arrow schema from current sql_schema + col_types and open the writer.
    fn resolve_schema(&mut self) -> anyhow::Result<()> {
        if self.arrow_schema.is_some() {
            return Ok(());
        }

        let fields: Vec<Field> = if self.sql_schema.columns.is_empty() {
            // No CREATE TABLE — generate synthetic column names.
            self.col_types
                .iter()
                .enumerate()
                .map(|(i, t)| Field::new(format!("col{i}"), inferred_to_arrow(t), true))
                .collect()
        } else {
            self.sql_schema
                .columns
                .iter()
                .zip(
                    self.col_types
                        .iter()
                        .chain(std::iter::repeat(&InferredType::Unknown)),
                )
                .map(|(col, t)| Field::new(&col.name, inferred_to_arrow(t), true))
                .collect()
        };

        let arrow_schema = Arc::new(ArrowSchema::new(fields));
        self.arrow_schema = Some(arrow_schema.clone());

        let file = File::create(&self.path)?;
        let props = WriterProperties::builder().build();
        self.writer = Some(ArrowWriter::try_new(file, arrow_schema, Some(props))?);
        Ok(())
    }

    /// Build a RecordBatch from pending rows and write it, then clear pending.
    fn flush_pending(&mut self) -> anyhow::Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let schema = self
            .arrow_schema
            .as_ref()
            .expect("arrow_schema must be set before flush")
            .clone();
        let batch = rows_to_record_batch(&schema, &self.pending)?;
        self.writer
            .as_mut()
            .expect("writer must be open before flush")
            .write(&batch)?;
        self.pending.clear();
        Ok(())
    }
}

impl Writer for ParquetWriter {
    fn write_header(&mut self, schema: &Schema) -> anyhow::Result<()> {
        self.sql_schema = schema.clone();
        self.col_types = schema
            .columns
            .iter()
            .map(|c| c.inferred_type.clone())
            .collect();

        // If CREATE TABLE gave us complete types, open the writer immediately.
        if self.all_types_known() {
            self.resolve_schema()?;
        }
        Ok(())
    }

    fn write_row(&mut self, schema: &Schema, row: &Row) -> anyhow::Result<()> {
        // If write_header was called with an empty schema but we now have one, adopt it.
        if self.sql_schema.columns.is_empty() && !schema.columns.is_empty() {
            self.sql_schema = schema.clone();
            self.col_types = schema
                .columns
                .iter()
                .map(|c| c.inferred_type.clone())
                .collect();
        }

        // Inference phase: widen column types to accommodate this row's values.
        if self.arrow_schema.is_none() {
            while self.col_types.len() < row.values.len() {
                self.col_types.push(InferredType::Unknown);
            }
            for (i, val) in row.values.iter().enumerate() {
                self.col_types[i] = self.col_types[i].widen_to_fit(val);
            }
        }

        self.pending.push(row.clone());

        if self.arrow_schema.is_none() {
            // Resolve schema once we know all types or have seen enough rows.
            if self.all_types_known() || self.pending.len() >= self.infer_rows {
                self.resolve_schema()?;
                self.flush_pending()?;
            }
        } else if self.pending.len() >= self.batch_size {
            self.flush_pending()?;
        }

        Ok(())
    }

    fn finish(&mut self) -> anyhow::Result<()> {
        // Schema may still be unresolved if all rows arrived before infer_rows.
        if self.arrow_schema.is_none() {
            // If we never got any column info, synthesise names from the first row.
            if self.sql_schema.columns.is_empty() {
                let n = self.pending.first().map_or(0, |r| r.values.len());
                self.col_types.resize(n, InferredType::Unknown);
            }
            self.resolve_schema()?;
        }
        self.flush_pending()?;
        if let Some(writer) = self.writer.take() {
            writer.close()?;
        }
        Ok(())
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn inferred_to_arrow(t: &InferredType) -> DataType {
    match t {
        InferredType::Boolean => DataType::Boolean,
        InferredType::Int64   => DataType::Int64,
        InferredType::Float64 => DataType::Float64,
        _                     => DataType::Utf8,   // Unknown and Utf8 → string
    }
}

fn rows_to_record_batch(
    arrow_schema: &Arc<ArrowSchema>,
    rows: &[Row],
) -> anyhow::Result<RecordBatch> {
    if arrow_schema.fields().is_empty() && !rows.is_empty() {
        anyhow::bail!(
            "cannot write {} row(s): schema has 0 columns (no CREATE TABLE and no rows to infer from)",
            rows.len()
        );
    }
    let mut cols: Vec<ArrayRef> = Vec::with_capacity(arrow_schema.fields().len());

    for (i, field) in arrow_schema.fields().iter().enumerate() {
        let arr: ArrayRef = match field.data_type() {
            DataType::Boolean => {
                let mut b = BooleanBuilder::with_capacity(rows.len());
                for row in rows {
                    match row.values.get(i) {
                        Some(Value::Bool(v)) => b.append_value(*v),
                        _                    => b.append_null(),
                    }
                }
                Arc::new(b.finish())
            }
            DataType::Int64 => {
                let mut b = Int64Builder::with_capacity(rows.len());
                for row in rows {
                    match row.values.get(i) {
                        Some(Value::Integer(v)) => b.append_value(*v),
                        Some(Value::Bool(v))    => b.append_value(*v as i64),
                        _                       => b.append_null(),
                    }
                }
                Arc::new(b.finish())
            }
            DataType::Float64 => {
                let mut b = Float64Builder::with_capacity(rows.len());
                for row in rows {
                    match row.values.get(i) {
                        Some(Value::Float(v))   => b.append_value(*v),
                        Some(Value::Integer(v)) => b.append_value(*v as f64),
                        _                       => b.append_null(),
                    }
                }
                Arc::new(b.finish())
            }
            _ => {
                let mut b = StringBuilder::with_capacity(rows.len(), rows.len() * 16);
                for row in rows {
                    match row.values.get(i) {
                        Some(Value::Null) | None => b.append_null(),
                        Some(v)                  => b.append_value(v.to_string()),
                    }
                }
                Arc::new(b.finish())
            }
        };
        cols.push(arr);
    }

    Ok(RecordBatch::try_new(arrow_schema.clone(), cols)?)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{Column, InferredType, Row, Schema, Value};
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

    fn read_parquet(path: &Path) -> Vec<RecordBatch> {
        let file = File::open(path).unwrap();
        ParquetRecordBatchReaderBuilder::try_new(file)
            .unwrap()
            .build()
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    }

    fn total_rows(batches: &[RecordBatch]) -> usize {
        batches.iter().map(|b| b.num_rows()).sum()
    }

    fn typed_schema() -> Schema {
        Schema {
            table_name: "t".into(),
            columns: vec![
                Column { name: "id".into(),     inferred_type: InferredType::Int64 },
                Column { name: "name".into(),   inferred_type: InferredType::Utf8 },
                Column { name: "active".into(), inferred_type: InferredType::Boolean },
                Column { name: "score".into(),  inferred_type: InferredType::Float64 },
            ],
        }
    }

    #[test]
    fn writes_and_reads_with_typed_schema() {
        let path = std::env::temp_dir().join("skim_test_typed.parquet");
        let schema = typed_schema();

        let mut w = ParquetWriter::new(&path, 1000, 100).unwrap();
        w.write_header(&schema).unwrap();
        w.write_row(&schema, &Row { values: vec![
            Value::Integer(1), Value::Text("Alice".into()),
            Value::Bool(true), Value::Float(9.5),
        ]}).unwrap();
        w.write_row(&schema, &Row { values: vec![
            Value::Integer(2), Value::Text("Bob".into()),
            Value::Bool(false), Value::Null,
        ]}).unwrap();
        w.finish().unwrap();

        let batches = read_parquet(&path);
        assert_eq!(total_rows(&batches), 2);

        let batch = &batches[0];
        assert_eq!(batch.schema().field(0).name(), "id");
        assert_eq!(batch.schema().field(0).data_type(), &DataType::Int64);
        assert_eq!(batch.schema().field(1).name(), "name");
        assert_eq!(batch.schema().field(2).name(), "active");
        assert_eq!(batch.schema().field(3).name(), "score");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn infers_schema_from_rows() {
        let path = std::env::temp_dir().join("skim_test_infer.parquet");
        // Empty schema — types must be inferred from rows.
        let schema = Schema { table_name: "t".into(), columns: vec![] };

        let mut w = ParquetWriter::new(&path, 1000, 10).unwrap();
        w.write_header(&schema).unwrap();
        w.write_row(&schema, &Row { values: vec![Value::Integer(1), Value::Text("x".into())] }).unwrap();
        w.write_row(&schema, &Row { values: vec![Value::Integer(2), Value::Text("y".into())] }).unwrap();
        w.finish().unwrap();

        let batches = read_parquet(&path);
        assert_eq!(total_rows(&batches), 2);
        // Columns should have synthetic names and inferred types.
        let batch = &batches[0];
        assert_eq!(batch.schema().field(0).name(), "col0");
        assert_eq!(batch.schema().field(0).data_type(), &DataType::Int64);
        assert_eq!(batch.schema().field(1).name(), "col1");
        assert_eq!(batch.schema().field(1).data_type(), &DataType::Utf8);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn batch_flushing() {
        // batch_size=2 forces multiple RecordBatch writes.
        let path = std::env::temp_dir().join("skim_test_batch.parquet");
        let schema = Schema {
            table_name: "t".into(),
            columns: vec![Column { name: "n".into(), inferred_type: InferredType::Int64 }],
        };

        let mut w = ParquetWriter::new(&path, 2, 100).unwrap();
        w.write_header(&schema).unwrap();
        for i in 0i64..5 {
            w.write_row(&schema, &Row { values: vec![Value::Integer(i)] }).unwrap();
        }
        w.finish().unwrap();

        let batches = read_parquet(&path);
        assert_eq!(total_rows(&batches), 5);

        let _ = std::fs::remove_file(&path);
    }
}
