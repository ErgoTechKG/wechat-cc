use thiserror::Error;

#[derive(Error, Debug)]
pub enum BridgeError {
    #[error("Config error: {0}")]
    Config(String),

    #[error("Database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("Docker error: {0}")]
    Docker(String),

    #[error("WeChat bot error: {0}")]
    WeChat(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("{0}")]
    Other(#[from] anyhow::Error),
}

pub type Result<T> = std::result::Result<T, BridgeError>;
