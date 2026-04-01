use thiserror::Error;

#[derive(Debug, Error)]
pub enum DomainError {
    #[error("Not Found: {0}")]
    NotFound(String),
    #[error("Invalid State: {0}")]
    InvalidState(String),
}
