// Phase 5c — stub
use crate::parser::{Row, Schema};
use super::Writer;

pub struct CsvWriter<W: std::io::Write> {
    _out: W,
    _no_header: bool,
}

impl<W: std::io::Write> CsvWriter<W> {
    pub fn new(out: W, no_header: bool) -> Self { Self { _out: out, _no_header: no_header } }
}

impl<W: std::io::Write> Writer for CsvWriter<W> {
    fn write_header(&mut self, _schema: &Schema) -> anyhow::Result<()> { Ok(()) }
    fn write_row(&mut self, _schema: &Schema, _row: &Row) -> anyhow::Result<()> { Ok(()) }
    fn finish(&mut self) -> anyhow::Result<()> { Ok(()) }
}
