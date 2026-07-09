use thiserror::Error;

/// Crate-wide error type. Library/service functions return `Result<T>`;
/// the SSH handler and `main` bridge these into `anyhow::Error`.
#[derive(Debug, Error)]
pub enum AppError {
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),

    #[error("migration error: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),

    #[error("password hashing failed: {0}")]
    Hash(String),

    #[error("username already taken")]
    UsernameTaken,

    #[error("that action is not available to the guest account")]
    GuestNotAllowed,

    #[error("recipient not found")]
    RecipientNotFound,

    #[error("not found")]
    NotFound,

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub type Result<T> = std::result::Result<T, AppError>;
