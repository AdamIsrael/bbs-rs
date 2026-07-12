-- Public-key SSH auth: registered users may attach SSH public keys and
-- authenticate with them (in addition to their password).
--
-- `public_key` is the canonical OpenSSH encoding (algorithm + base64, no
-- comment); `fingerprint` is the SHA256 fingerprint used for auth matching.
-- A key is unique per user (a given key may be registered by more than one
-- user, each authorizing their own account).

CREATE TABLE user_keys (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id     INTEGER NOT NULL REFERENCES users(id),
    algorithm   TEXT    NOT NULL,
    fingerprint TEXT    NOT NULL,
    public_key  TEXT    NOT NULL,
    label       TEXT    NOT NULL DEFAULT '',
    created_at  INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX idx_user_keys_user ON user_keys (user_id);
CREATE UNIQUE INDEX idx_user_keys_fpr ON user_keys (user_id, fingerprint);
