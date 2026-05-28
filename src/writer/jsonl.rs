// Phase 5a — stub
use crate::parser::{Row, Schema};
use super::Writer;

pub struct JsonlWriter<W: std::io::Write> {
    _out: W,
}

impl<W: std::io::Write> JsonlWriter<W> {
    pub fn new(out: W) -> Self { Self { _out: out } }
}

impl<W: std::io::Write> Writer for JsonlWriter<W> {
    fn write_header(&mut self, _schema: &Schema) -> anyhow::Result<()> { Ok(()) }
    fn write_row(&mut self, _schema: &Schema, _row: &Row) -> anyhow::Result<()> { Ok(()) }
    fn finish(&mut self) -> anyhow::Result<()> { Ok(()) }
}
