-- File areas: named download areas (like message boards, but for files).
-- Phase 1 (catalog): browsable metadata + FS-backed storage; operators add
-- files via bbsctl. Live user upload/download over SFTP is a follow-up.
--
-- Areas carry the same role ACL as boards (read/write min role); `files` rows
-- record metadata plus a `storage_path` relative to the configured files dir.

CREATE TABLE file_areas (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    name           TEXT    NOT NULL UNIQUE,
    description    TEXT    NOT NULL DEFAULT '',
    min_read_role  TEXT    NOT NULL DEFAULT 'guest',
    min_write_role TEXT    NOT NULL DEFAULT 'user',
    created_at     INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE files (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    area_id      INTEGER NOT NULL REFERENCES file_areas(id),
    uploader_id  INTEGER NOT NULL REFERENCES users(id),
    filename     TEXT    NOT NULL,
    description  TEXT    NOT NULL DEFAULT '',
    size         INTEGER NOT NULL DEFAULT 0,
    storage_path TEXT    NOT NULL,
    downloads    INTEGER NOT NULL DEFAULT 0,
    created_at   INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX idx_files_area ON files (area_id, id);
CREATE INDEX idx_files_uploader ON files (uploader_id);
