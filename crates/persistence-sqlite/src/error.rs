use thiserror::Error;

#[derive(Debug, Error)]
pub enum PersistenceError {
    #[error("database operation failed: {0}")]
    Database(#[from] sqlx::Error),

    #[error("database migration failed: {0}")]
    Migration(#[from] sqlx::migrate::MigrateError),

    #[error("reminder revision conflict for {id}; expected revision {expected_revision}")]
    RevisionConflict { id: String, expected_revision: i64 },

    #[error("persistence invariant violated: {0}")]
    InvariantViolation(String),
}

pub type Result<T> = std::result::Result<T, PersistenceError>;
