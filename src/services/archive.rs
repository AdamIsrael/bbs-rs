//! Bounded, read-only inspection of stored files for the in-BBS viewer:
//! decode text files, and list / read entries of `.zip`, `.tar.gz`/`.tgz`, and
//! `.gz` archives. Everything is streamed from the blob and capped by the
//! `[files]` preview limits — nothing is extracted to disk.

use std::cmp::Ordering;
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

/// Order entries with directories first, then by natural name order within each
/// group (see [`natural_cmp`]).
fn sort_entries(entries: &mut [ArchiveEntry]) {
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| natural_cmp(&a.name, &b.name))
    });
}

/// Consume a run of ASCII digits from the front of `it`.
fn take_digits(it: &mut std::iter::Peekable<std::str::Chars>) -> String {
    let mut s = String::new();
    while let Some(&c) = it.peek() {
        if c.is_ascii_digit() {
            s.push(c);
            it.next();
        } else {
            break;
        }
    }
    s
}

/// Compare two runs of digits by numeric value (ignoring leading zeros), so
/// `2` precedes `10`. Ties break toward fewer leading zeros for determinism.
fn cmp_numeric(a: &str, b: &str) -> Ordering {
    let ta = a.trim_start_matches('0');
    let tb = b.trim_start_matches('0');
    ta.len()
        .cmp(&tb.len())
        .then_with(|| ta.cmp(tb))
        .then_with(|| a.len().cmp(&b.len()))
}

/// Natural ("human") comparison: digit runs compare numerically so `2.txt`
/// sorts before `10.txt`; other characters compare case-insensitively.
fn natural_cmp(a: &str, b: &str) -> Ordering {
    let mut ai = a.chars().peekable();
    let mut bi = b.chars().peekable();
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(ca), Some(cb)) if ca.is_ascii_digit() && cb.is_ascii_digit() => {
                let da = take_digits(&mut ai);
                let db = take_digits(&mut bi);
                match cmp_numeric(&da, &db) {
                    Ordering::Equal => {}
                    ord => return ord,
                }
            }
            (Some(ca), Some(cb)) => match ca.to_ascii_lowercase().cmp(&cb.to_ascii_lowercase()) {
                Ordering::Equal => {
                    ai.next();
                    bi.next();
                }
                ord => return ord,
            },
        }
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
    sort_entries(&mut entries);
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
    sort_entries(&mut entries);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sorted(names: &[&str]) -> Vec<String> {
        let mut v: Vec<ArchiveEntry> = names
            .iter()
            .map(|n| ArchiveEntry {
                name: n.to_string(),
                size: 0,
                is_dir: n.ends_with('/'),
            })
            .collect();
        sort_entries(&mut v);
        v.into_iter().map(|e| e.name).collect()
    }

    #[test]
    fn natural_order_puts_10_after_9() {
        let out = sorted(&["10.txt", "1.txt", "2.txt", "9.txt"]);
        assert_eq!(out, ["1.txt", "2.txt", "9.txt", "10.txt"]);
    }

    #[test]
    fn dirs_group_first_then_natural() {
        // "img10/" is a dir (sorts among dirs); files follow, numerically.
        let out = sorted(&["img2/", "file10.log", "file2.log", "img10/", "a.txt"]);
        assert_eq!(out, ["img2/", "img10/", "a.txt", "file2.log", "file10.log"]);
    }

    #[test]
    fn case_insensitive_and_leading_zeros() {
        assert_eq!(natural_cmp("Apple", "apple"), Ordering::Equal);
        assert_eq!(natural_cmp("v1", "v01"), Ordering::Less); // fewer zeros first on tie
        assert_eq!(natural_cmp("x2", "x10"), Ordering::Less);
    }
}
