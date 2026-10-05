use thiserror::Error;

/// 核心库统一错误。
#[derive(Debug, Error)]
pub enum Error {
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("crypto error: {0}")]
    Crypto(String),
    #[error("invalid block: {0}")]
    InvalidBlock(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("format error: {0}")]
    Format(String),
}

pub type Result<T> = std::result::Result<T, Error>;
