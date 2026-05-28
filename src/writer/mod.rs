pub mod csv;
pub mod json;
pub mod jsonl;
pub mod parquet;
pub mod yaml;

use crate::parser::{Row, Schema};

/// Common interface for all output format writers.
pub trait Writer {
    /// Called once before any rows, with the final resolved schema.
    fn write_header(&mut self, schema: &Schema) -> anyhow::Result<()>;

    /// Called for each row. `schema` is passed again for column name lookups.
    fn write_row(&mut self, schema: &Schema, row: &Row) -> anyhow::Result<()>;

    /// Called once at the end. Must flush all buffers and finalise the output.
    fn finish(&mut self) -> anyhow::Result<()>;
}
