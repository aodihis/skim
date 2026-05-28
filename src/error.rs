use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConvertError {
    #[error("SQL statement exceeds maximum size of {max_bytes} bytes (got {actual_bytes})")]
    StatementTooLarge { max_bytes: usize, actual_bytes: usize },

    #[error("Parquet requires a real file path, not stdout")]
    ParquetRequiresFile,

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}
