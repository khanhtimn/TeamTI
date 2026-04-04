pub mod db;
pub mod migrations;
pub mod repositories;

/// Macro: convert sqlx::Error to AppError::Persistence via opaque PersistenceError.
/// The conversion from sqlx::Error to PersistenceKind lives here (in the adapter),
/// not in the application layer — fulfilling the hexagonal boundary.
#[macro_export]
macro_rules! db_err {
    ($op:literal) => {
        |e: sqlx::Error| {
            let kind = match &e {
                sqlx::Error::PoolTimedOut | sqlx::Error::PoolClosed => {
                    application::error::PersistenceKind::PoolExhausted
                }
                sqlx::Error::Io(_) => application::error::PersistenceKind::ConnectionLost,
                sqlx::Error::RowNotFound => application::error::PersistenceKind::NotFound,
                sqlx::Error::Database(db_err) if db_err.is_unique_violation() => {
                    application::error::PersistenceKind::ConstraintViolation
                }
                _ => application::error::PersistenceKind::Other,
            };
            application::error::PersistenceError::new($op, kind, e.to_string(), Some(Box::new(e)))
        }
    };
}
