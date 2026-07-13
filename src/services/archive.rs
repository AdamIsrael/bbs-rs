//! Bounded, read-only inspection of stored files for the in-BBS viewer:
//! decode text files, and list / read entries of `.zip`, `.tar.gz`/`.tgz`, and
//! `.gz` archives. Everything is streamed from the blob and capped by the
//! `[files]` preview limits — nothing is extracted to disk.

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use flate2::read::GzDecoder;

use crate::config::Files;
use crate::error::{AppError, Result};

/// One entry inside an archive.
pub struct ArchiveEntry {
    pub name: String,
    pub size: u64,
    pub is_dir: bool,
}

/// The result of inspecting a file (or an archive entry).
pub enum Preview {
    /// Decoded UTF-8 text (possibly truncated to the preview cap).
    Text { content: String, truncated: bool },
    /// Not UTF-8 text (binary) — offer SFTP download instead.
    Binary,
    /// An archive's entry listing (possibly capped).
    Archive {
        entries: Vec<ArchiveEntry>,
        truncated: bool,
    },
}

/// How a filename should be handled.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Zip,
    TarGz,
    Gz,
    Plain,
}

/// Classify a filename by extension.
pub fn kind_of(filename: &str) -> Kind {
    let f = filename.to_ascii_lowercase();
    if f.ends_with(".zip") {
        Kind::Zip
    } else if f.ends_with(".tar.gz") || f.ends_with(".tgz") {
        Kind::TarGz
    } else if f.ends_with(".gz") {
        Kind::Gz
    } else {
        Kind::Plain
    }
}

/// Whether the file is a multi-entry archive (zip / tar.gz) — i.e. it lists
/// entries rather than previewing as a single stream. `.gz` is single-member.
pub fn is_archive(filename: &str) -> bool {
    matches!(kind_of(filename), Kind::Zip | Kind::TarGz)
}

fn io_err(e: impl std::fmt::Display) -> AppError {
    AppError::Other(anyhow::anyhow!("{e}"))
}

/// Read at most `max` bytes, reporting whether more remained.
fn read_bounded(mut r: impl Read, max: u64) -> io::Result<(Vec<u8>, bool)> {
    let mut buf = Vec::new();
    r.by_ref()
        .take(max.saturating_add(1))
        .read_to_end(&mut buf)?;
    let truncated = buf.len() as u64 > max;
    if truncated {
        buf.truncate(max as usize);
    }
    Ok((buf, truncated))
}

/// Decide whether the bytes are text (and decode) or binary. A NUL byte or
/// invalid UTF-8 means binary — except an incomplete trailing multibyte
/// sequence caused by truncation, which is trimmed.
fn classify(mut bytes: Vec<u8>, truncated: bool) -> Preview {
    if bytes.contains(&0) {
        return Preview::Binary;
    }
    if let Err(e) = std::str::from_utf8(&bytes) {
        if truncated && e.error_len().is_none() {
            bytes.truncate(e.valid_up_to());
        } else {
            return Preview::Binary;
        }
    }
    match String::from_utf8(bytes) {
        Ok(content) => Preview::Text { content, truncated },
        Err(_) => Preview::Binary,
    }
}

fn list_zip(path: &Path, cfg: &Files) -> Result<Preview> {
    let file = File::open(path).map_err(io_err)?;
    let mut zip = zip::ZipArchive::new(file).map_err(io_err)?;
    let mut entries = Vec::new();
    let mut truncated = false;
    for i in 0..zip.len() {
        if entries.len() >= cfg.max_archive_entries {
            truncated = true;
            break;
        }
        let f = zip.by_index(i).map_err(io_err)?;
        entries.push(ArchiveEntry {
            name: f.name().to_string(),
            size: f.size(),
            is_dir: f.is_dir(),
        });
    }
    Ok(Preview::Archive { entries, truncated })
}

fn list_tar_gz(path: &Path, cfg: &Files) -> Result<Preview> {
    let file = File::open(path).map_err(io_err)?;
    let mut ar = tar::Archive::new(GzDecoder::new(file));
    let mut entries = Vec::new();
    let mut truncated = false;
    for e in ar.entries().map_err(io_err)? {
        if entries.len() >= cfg.max_archive_entries {
            truncated = true;
            break;
        }
        let e = e.map_err(io_err)?;
        let name = e.path().map_err(io_err)?.to_string_lossy().into_owned();
        let is_dir = e.header().entry_type().is_dir();
        let size = e.header().size().unwrap_or(0);
        entries.push(ArchiveEntry { name, size, is_dir });
    }
    Ok(Preview::Archive { entries, truncated })
}

/// Inspect a stored file: an archive listing (zip / tar.gz), a decompressed
/// single stream (`.gz`), or a plain file's text.
pub fn inspect(path: &Path, filename: &str, cfg: &Files) -> Result<Preview> {
    match kind_of(filename) {
        Kind::Zip => list_zip(path, cfg),
        Kind::TarGz => list_tar_gz(path, cfg),
        Kind::Gz => {
            let file = File::open(path).map_err(io_err)?;
            let (bytes, truncated) =
                read_bounded(GzDecoder::new(file), cfg.max_preview_bytes).map_err(io_err)?;
            Ok(classify(bytes, truncated))
        }
        Kind::Plain => {
            let file = File::open(path).map_err(io_err)?;
            let (bytes, truncated) = read_bounded(file, cfg.max_preview_bytes).map_err(io_err)?;
            Ok(classify(bytes, truncated))
        }
    }
}

/// Read a named entry from a zip / tar.gz archive as a bounded text preview.
pub fn read_entry(path: &Path, filename: &str, entry: &str, cfg: &Files) -> Result<Preview> {
    match kind_of(filename) {
        Kind::Zip => {
            let file = File::open(path).map_err(io_err)?;
            let mut zip = zip::ZipArchive::new(file).map_err(io_err)?;
            let zf = zip.by_name(entry).map_err(|_| AppError::NotFound)?;
            let (bytes, truncated) = read_bounded(zf, cfg.max_preview_bytes).map_err(io_err)?;
            Ok(classify(bytes, truncated))
        }
        Kind::TarGz => {
            let file = File::open(path).map_err(io_err)?;
            let mut ar = tar::Archive::new(GzDecoder::new(file));
            for e in ar.entries().map_err(io_err)? {
                let mut e = e.map_err(io_err)?;
                let name = e.path().map_err(io_err)?.to_string_lossy().into_owned();
                if name == entry {
                    let (bytes, truncated) =
                        read_bounded(&mut e, cfg.max_preview_bytes).map_err(io_err)?;
                    return Ok(classify(bytes, truncated));
                }
            }
            Err(AppError::NotFound)
        }
        Kind::Gz | Kind::Plain => Err(AppError::NotFound),
    }
}
