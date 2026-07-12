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

    #[error("that SSH key is already registered")]
    KeyExists,

    #[error("invalid SSH public key: {0}")]
    InvalidKey(String),

    #[error("that username is reserved")]
    UsernameReserved,

    #[error("that action is not available to the guest account")]
    GuestNotAllowed,

    #[error("oneliner must be 1–{0} characters")]
    OnelinerLength(usize),

    #[error("you're doing that too quickly — please slow down")]
    RateLimited,

    #[error("this board is locked")]
    BoardLocked,

    #[error("you don't have permission to post to this board")]
    BoardWriteDenied,

    #[error("recipient not found")]
    RecipientNotFound,

    #[error("not found")]
    NotFound,

    #[error("invalid role (expected guest, user, or admin): {0}")]
    BadRole(String),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub type Result<T> = std::result::Result<T, AppError>;
