use thiserror::Error;

#[derive(Debug, Error)]
pub enum TypeError {
    #[error("short read: needed {needed} bytes, have {have}")]
    ShortRead { needed: usize, have: usize },

    #[error("invalid UTF-8: {0}")]
    InvalidUtf8(String),

    #[error("value out of range: {0}")]
    OutOfRange(String),

    #[error("unknown type: {0}")]
    UnknownType(String),

    #[error("type mismatch: expected {expected}, got {actual}")]
    TypeMismatch { expected: String, actual: String },
}
