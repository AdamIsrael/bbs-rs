//! SSH public-key parsing and fingerprinting — the one place that understands
//! the OpenSSH key format. Keeps `services::keys` free of the `ssh-key` crate.

use russh::keys::ssh_key::{HashAlg, PublicKey};
use sqlx::sqlite::SqlitePool;

use crate::error::{AppError, Result};
use crate::services::keys;

/// A parsed public key, decomposed into the pieces we store.
pub struct ParsedKey {
    /// SSH algorithm name, e.g. `ssh-ed25519`.
    pub algorithm: String,
    /// SHA256 fingerprint, e.g. `SHA256:…`.
    pub fingerprint: String,
    /// Canonical OpenSSH encoding (algorithm + base64, comment stripped).
    pub canonical: String,
    /// The key's comment, if any (used as a default label).
    pub comment: String,
}

/// Parse an OpenSSH public-key line (`ssh-ed25519 AAAA… comment`).
pub fn parse(line: &str) -> Result<ParsedKey> {
    let mut key =
        PublicKey::from_openssh(line.trim()).map_err(|e| AppError::InvalidKey(e.to_string()))?;
    let algorithm = key.algorithm().as_str().to_string();
    let fingerprint = key.fingerprint(HashAlg::Sha256).to_string();
    let comment = key.comment().to_string();
    key.set_comment("");
    let canonical = key
        .to_openssh()
        .map_err(|e| AppError::InvalidKey(e.to_string()))?;
    Ok(ParsedKey {
        algorithm,
        fingerprint,
        canonical,
        comment,
    })
}

/// The SHA256 fingerprint of a key offered during SSH auth.
pub fn fingerprint(key: &PublicKey) -> String {
    key.fingerprint(HashAlg::Sha256).to_string()
}

/// Parse `line` and register it for `user_id`. `label` overrides the key's
/// comment when non-empty. Returns the parsed key (for a confirmation message).
pub async fn register(
    pool: &SqlitePool,
    user_id: i64,
    line: &str,
    label: &str,
) -> Result<ParsedKey> {
    let parsed = parse(line)?;
    let label = if label.trim().is_empty() {
        parsed.comment.clone()
    } else {
        label.trim().to_string()
    };
    keys::add_key(
        pool,
        user_id,
        &parsed.algorithm,
        &parsed.fingerprint,
        &parsed.canonical,
        &label,
    )
    .await?;
    Ok(parsed)
}
