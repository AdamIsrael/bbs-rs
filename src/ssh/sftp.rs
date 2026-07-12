//! An SFTP subsystem that exposes the file areas as a small virtual filesystem:
//!
//! ```text
//! /                     the areas the user may read, as directories
//! /<area>/              the files in an area
//! /<area>/<filename>    a file (download to read, upload to create)
//! ```
//!
//! Reads honor each area's `min_read_role`; uploads honor `min_write_role` plus
//! the `[files]` extension allowlist, per-file size cap, and per-user quota
//! (admins are quota-exempt, matching [`crate::services::files::add_file`]).

use std::collections::HashMap;
use std::sync::Arc;

use russh_sftp::protocol::{
    Attrs, Data, File, FileAttributes, Handle, Name, OpenFlags, Status, StatusCode, Version,
};
use sqlx::sqlite::SqlitePool;

use crate::config::Settings;
use crate::db::models::User;
use crate::error::AppError;
use crate::services::files;

/// Per-open-file state behind an SFTP handle.
enum Node {
    /// A directory listing, sent in a single `readdir` then EOF.
    Dir { entries: Vec<File>, sent: bool },
    /// An open download: the whole blob buffered in memory, plus the file id
    /// (so the download counter can be bumped on close).
    Read { file_id: i64, data: Vec<u8> },
    /// An in-progress upload accumulating bytes until close.
    Write {
        area_id: i64,
        filename: String,
        buf: Vec<u8>,
    },
}

/// One SFTP session, bound to the authenticated user.
pub struct SftpSession {
    pool: SqlitePool,
    config: Arc<Settings>,
    user: User,
    version: Option<u32>,
    handles: HashMap<String, Node>,
    counter: u64,
}

impl SftpSession {
    pub fn new(pool: SqlitePool, config: Arc<Settings>, user: User) -> Self {
        Self {
            pool,
            config,
            user,
            version: None,
            handles: HashMap::new(),
            counter: 0,
        }
    }

    fn new_handle(&mut self, node: Node) -> String {
        self.counter += 1;
        let key = format!("h{}", self.counter);
        self.handles.insert(key.clone(), node);
        key
    }
}

/// Normalize an SFTP path to an absolute, `.`/`..`-resolved form.
fn normalize(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    format!("/{}", parts.join("/"))
}

/// The non-empty path components (`/a/b` → `["a", "b"]`).
fn components(path: &str) -> Vec<String> {
    normalize(path)
        .split('/')
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

fn dir_attrs() -> FileAttributes {
    let mut a = FileAttributes {
        permissions: Some(0o755),
        ..Default::default()
    };
    a.set_dir(true);
    a
}

fn file_attrs(size: i64, mtime: i64) -> FileAttributes {
    let mut a = FileAttributes {
        size: Some(size.max(0) as u64),
        mtime: Some(mtime.max(0) as u32),
        permissions: Some(0o644),
        ..Default::default()
    };
    a.set_regular(true);
    a
}

fn ok_status(id: u32) -> Status {
    Status {
        id,
        status_code: StatusCode::Ok,
        error_message: "Ok".to_string(),
        language_tag: "en-US".to_string(),
    }
}

impl SftpSession {
    /// Finalize an upload: validate (extension / size / quota / write ACL was
    /// checked at open) and persist both the row and the blob.
    async fn finish_upload(
        &self,
        area_id: i64,
        filename: &str,
        buf: Vec<u8>,
    ) -> Result<(), StatusCode> {
        let entry = files::add_file(
            &self.pool,
            area_id,
            &self.user,
            filename,
            "",
            buf.len() as i64,
            &self.config.files,
        )
        .await
        .map_err(|e| match e {
            AppError::ExtensionNotAllowed => StatusCode::PermissionDenied,
            AppError::FileTooLarge(_) | AppError::QuotaExceeded(_) => StatusCode::Failure,
            _ => StatusCode::Failure,
        })?;

        let dest = self.config.files.storage_dir.join(&entry.storage_path);
        if let Some(parent) = dest.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        if tokio::fs::write(&dest, &buf).await.is_err() {
            // Roll the row back so quota accounting stays honest.
            let _ = files::delete_file(&self.pool, entry.id).await;
            return Err(StatusCode::Failure);
        }
        tracing::info!(
            "sftp: {} uploaded {} ({} bytes)",
            self.user.username,
            filename,
            buf.len()
        );
        Ok(())
    }
}

impl russh_sftp::server::Handler for SftpSession {
    type Error = StatusCode;

    fn unimplemented(&self) -> Self::Error {
        StatusCode::OpUnsupported
    }

    async fn init(
        &mut self,
        version: u32,
        _extensions: HashMap<String, String>,
    ) -> Result<Version, Self::Error> {
        if self.version.is_some() {
            return Err(StatusCode::ConnectionLost);
        }
        self.version = Some(version);
        Ok(Version::new())
    }

    async fn realpath(&mut self, id: u32, path: String) -> Result<Name, Self::Error> {
        Ok(Name {
            id,
            files: vec![File::dummy(normalize(&path))],
        })
    }

    async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, Self::Error> {
        let comps = components(&path);
        let entries: Vec<File> = match comps.as_slice() {
            [] => files::list_readable_areas(&self.pool, &self.user.role)
                .await
                .map_err(|_| StatusCode::Failure)?
                .into_iter()
                .map(|a| File::new(a.name, dir_attrs()))
                .collect(),
            [area] => {
                let a = files::get_area_by_name(&self.pool, area)
                    .await
                    .map_err(|_| StatusCode::NoSuchFile)?;
                if !a.can_read(&self.user.role) {
                    return Err(StatusCode::PermissionDenied);
                }
                files::list_files(&self.pool, a.id)
                    .await
                    .map_err(|_| StatusCode::Failure)?
                    .into_iter()
                    .map(|f| File::new(f.filename, file_attrs(f.size, f.created_at)))
                    .collect()
            }
            _ => return Err(StatusCode::NoSuchFile),
        };
        let handle = self.new_handle(Node::Dir {
            entries,
            sent: false,
        });
        Ok(Handle { id, handle })
    }

    async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, Self::Error> {
        match self.handles.get_mut(&handle) {
            Some(Node::Dir { entries, sent }) if !*sent => {
                *sent = true;
                Ok(Name {
                    id,
                    files: entries.clone(),
                })
            }
            Some(Node::Dir { .. }) => Err(StatusCode::Eof),
            _ => Err(StatusCode::Failure),
        }
    }

    async fn open(
        &mut self,
        id: u32,
        filename: String,
        pflags: OpenFlags,
        _attrs: FileAttributes,
    ) -> Result<Handle, Self::Error> {
        let comps = components(&filename);
        let [area_name, name] = comps.as_slice() else {
            return Err(StatusCode::NoSuchFile);
        };
        let area = files::get_area_by_name(&self.pool, area_name)
            .await
            .map_err(|_| StatusCode::NoSuchFile)?;

        if pflags.contains(OpenFlags::WRITE) {
            if !area.can_write(&self.user.role) {
                return Err(StatusCode::PermissionDenied);
            }
            // Fail fast on disallowed types instead of after a full transfer.
            if !self.config.files.extension_allowed(name) {
                return Err(StatusCode::PermissionDenied);
            }
            let handle = self.new_handle(Node::Write {
                area_id: area.id,
                filename: name.clone(),
                buf: Vec::new(),
            });
            Ok(Handle { id, handle })
        } else {
            if !area.can_read(&self.user.role) {
                return Err(StatusCode::PermissionDenied);
            }
            let file = files::get_file_by_name(&self.pool, area.id, name)
                .await
                .map_err(|_| StatusCode::Failure)?
                .ok_or(StatusCode::NoSuchFile)?;
            let data = tokio::fs::read(self.config.files.storage_dir.join(&file.storage_path))
                .await
                .map_err(|_| StatusCode::NoSuchFile)?;
            let handle = self.new_handle(Node::Read {
                file_id: file.id,
                data,
            });
            Ok(Handle { id, handle })
        }
    }

    async fn read(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        len: u32,
    ) -> Result<Data, Self::Error> {
        match self.handles.get(&handle) {
            Some(Node::Read { data, .. }) => {
                let start = offset as usize;
                if start >= data.len() {
                    return Err(StatusCode::Eof);
                }
                let end = (start + len as usize).min(data.len());
                Ok(Data {
                    id,
                    data: data[start..end].to_vec(),
                })
            }
            _ => Err(StatusCode::Failure),
        }
    }

    async fn write(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        data: Vec<u8>,
    ) -> Result<Status, Self::Error> {
        match self.handles.get_mut(&handle) {
            Some(Node::Write { buf, .. }) => {
                let start = offset as usize;
                let end = start + data.len();
                if buf.len() < end {
                    buf.resize(end, 0);
                }
                buf[start..end].copy_from_slice(&data);
                Ok(ok_status(id))
            }
            _ => Err(StatusCode::Failure),
        }
    }

    async fn close(&mut self, id: u32, handle: String) -> Result<Status, Self::Error> {
        match self.handles.remove(&handle) {
            Some(Node::Write {
                area_id,
                filename,
                buf,
            }) => {
                self.finish_upload(area_id, &filename, buf).await?;
                Ok(ok_status(id))
            }
            Some(Node::Read { file_id, .. }) => {
                let _ = files::record_download(&self.pool, file_id).await;
                Ok(ok_status(id))
            }
            _ => Ok(ok_status(id)),
        }
    }

    async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        let comps = components(&path);
        let attrs = match comps.as_slice() {
            [] => dir_attrs(),
            [area] => {
                let a = files::get_area_by_name(&self.pool, area)
                    .await
                    .map_err(|_| StatusCode::NoSuchFile)?;
                if !a.can_read(&self.user.role) {
                    return Err(StatusCode::NoSuchFile);
                }
                dir_attrs()
            }
            [area, name] => {
                let a = files::get_area_by_name(&self.pool, area)
                    .await
                    .map_err(|_| StatusCode::NoSuchFile)?;
                if !a.can_read(&self.user.role) {
                    return Err(StatusCode::NoSuchFile);
                }
                let f = files::get_file_by_name(&self.pool, a.id, name)
                    .await
                    .map_err(|_| StatusCode::Failure)?
                    .ok_or(StatusCode::NoSuchFile)?;
                file_attrs(f.size, f.created_at)
            }
            _ => return Err(StatusCode::NoSuchFile),
        };
        Ok(Attrs { id, attrs })
    }

    async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        self.stat(id, path).await
    }

    async fn fstat(&mut self, id: u32, handle: String) -> Result<Attrs, Self::Error> {
        let attrs = match self.handles.get(&handle) {
            Some(Node::Read { data, .. }) => file_attrs(data.len() as i64, 0),
            Some(Node::Write { buf, .. }) => file_attrs(buf.len() as i64, 0),
            Some(Node::Dir { .. }) => dir_attrs(),
            None => return Err(StatusCode::Failure),
        };
        Ok(Attrs { id, attrs })
    }

    async fn remove(&mut self, id: u32, filename: String) -> Result<Status, Self::Error> {
        let comps = components(&filename);
        let [area_name, name] = comps.as_slice() else {
            return Err(StatusCode::NoSuchFile);
        };
        let area = files::get_area_by_name(&self.pool, area_name)
            .await
            .map_err(|_| StatusCode::NoSuchFile)?;
        let file = files::get_file_by_name(&self.pool, area.id, name)
            .await
            .map_err(|_| StatusCode::Failure)?
            .ok_or(StatusCode::NoSuchFile)?;
        // Only the uploader or an admin may delete.
        if file.uploader_id != self.user.id && !self.user.is_admin() {
            return Err(StatusCode::PermissionDenied);
        }
        if let Ok(Some(path)) = files::delete_file(&self.pool, file.id).await {
            let _ = tokio::fs::remove_file(self.config.files.storage_dir.join(path)).await;
        }
        Ok(ok_status(id))
    }

    // OpenSSH sets permissions/mtime after an upload; accept and ignore so the
    // client doesn't report a spurious failure.
    async fn setstat(
        &mut self,
        id: u32,
        _path: String,
        _attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        Ok(ok_status(id))
    }

    async fn fsetstat(
        &mut self,
        id: u32,
        _handle: String,
        _attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        Ok(ok_status(id))
    }
}
