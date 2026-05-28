// Phase 5e — stub
use crate::parser::{Row, Schema};
use super::Writer;

pub struct ParquetWriter {
    _batch_size: usize,
    _infer_rows: usize,
}

impl ParquetWriter {
    pub fn new(_path: &std::path::Path, batch_size: usize, infer_rows: usize) -> anyhow::Result<Self> {
        Ok(Self { _batch_size: batch_size, _infer_rows: infer_rows })
    }
}

impl Writer for ParquetWriter {
    fn write_header(&mut self, _schema: &Schema) -> anyhow::Result<()> { Ok(()) }
    fn write_row(&mut self, _schema: &Schema, _row: &Row) -> anyhow::Result<()> { Ok(()) }
    fn finish(&mut self) -> anyhow::Result<()> { Ok(()) }
}
