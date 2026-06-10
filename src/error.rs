use thiserror::Error;

pub type Result<T> = std::result::Result<T, EnviraError>;

#[derive(Debug, Error)]
pub enum EnviraError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
