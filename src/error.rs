use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConvertError {
    #[error("SQL statement exceeds maximum size of {max_bytes} bytes (got {actual_bytes})")]
    StatementTooLarge { max_bytes: usize, actual_bytes: usize },

    #[error("Failed to parse SQL statement: {reason}")]
    ParseError { reason: String },

    #[error("INSERT statement references column index {index} but only {count} columns are known")]
    ColumnIndexOutOfBounds { index: usize, count: usize },

    #[error("Parquet requires a real file path, not stdout")]
    ParquetRequiresFile,

    #[error("Schema not yet known — no CREATE TABLE or INSERT seen for table '{table}'")]
    SchemaUnknown { table: String },

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}
