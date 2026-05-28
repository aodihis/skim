pub mod schema;
pub mod state_machine;
pub mod value_parser;

/// SQL dialect used when parsing statements.
/// Determines which sqlparser dialect is used and how identifiers are handled.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SqlDialect {
    Mysql,
    Postgres,
}

// ── Shared data types ────────────────────────────────────────────────────────

/// A single SQL value after type coercion.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Integer(i64),
    Float(f64),
    Text(String),
    /// Raw bytes — from BLOB / hex literals.
    Bytes(Vec<u8>),
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::Null         => write!(f, "NULL"),
            Value::Bool(b)      => write!(f, "{b}"),
            Value::Integer(n)   => write!(f, "{n}"),
            Value::Float(v)     => write!(f, "{v}"),
            Value::Text(s)      => write!(f, "{s}"),
            Value::Bytes(b)     => write!(f, "<{} bytes>", b.len()),
        }
    }
}

/// A single row — values in the same order as `Schema::columns`.
#[derive(Debug, Clone)]
pub struct Row {
    pub values: Vec<Value>,
}

/// Column metadata (name + type hint for Parquet).
#[derive(Debug, Clone)]
pub struct Column {
    pub name: String,
    pub inferred_type: InferredType,
}

/// Type hierarchy used for Parquet schema inference.
/// Widening order: Unknown → Boolean → Int64 → Float64 → Utf8
#[derive(Debug, Clone, PartialEq)]
pub enum InferredType {
    Unknown,
    Boolean,
    Int64,
    Float64,
    Utf8,
}

impl InferredType {
    /// Widen `self` to accommodate `other`.
    pub fn widen_to_fit(&self, other: &Value) -> InferredType {
        let candidate = match other {
            Value::Null       => return self.clone(), // NULL is compatible with anything
            Value::Bool(_)    => InferredType::Boolean,
            Value::Integer(_) => InferredType::Int64,
            Value::Float(_)   => InferredType::Float64,
            Value::Text(_) | Value::Bytes(_) => InferredType::Utf8,
        };
        self.wider_of(&candidate)
    }

    fn wider_of(&self, other: &InferredType) -> InferredType {
        use InferredType::*;
        match (self, other) {
            (Utf8, _) | (_, Utf8)         => Utf8,
            (Float64, _) | (_, Float64)   => Float64,
            (Int64, _) | (_, Int64)       => Int64,
            (Boolean, _) | (_, Boolean)   => Boolean,
            // (Unknown, Unknown) falls here — returns Unknown
            (Unknown, t)                  => t.clone(),
        }
    }
}

/// Table schema: name + ordered list of columns.
#[derive(Debug, Clone)]
pub struct Schema {
    pub table_name: String,
    pub columns: Vec<Column>,
}

impl Schema {
    pub fn column_count(&self) -> usize {
        self.columns.len()
    }
}
