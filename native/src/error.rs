use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("URL error: {0}")]
    Url(#[from] url::ParseError),
    #[error("ZIP error: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("WebSocket error: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("authentication error: {0}")]
    Auth(String),
    #[error("API error: {0}")]
    Api(String),
    #[error("configuration error: {0}")]
    Config(String),
    #[error("binary error: {0}")]
    Binary(String),
    #[error("process error: {0}")]
    Process(String),
    #[error("Tauri error: {0}")]
    Tauri(#[from] tauri::Error),
    #[cfg(not(target_os = "windows"))]
    #[error("unsupported operation: {0}")]
    Unsupported(String),
}

impl Serialize for AppError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

pub type AppResult<T> = Result<T, AppError>;
