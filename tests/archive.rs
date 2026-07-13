//! Tests for the file-preview / archive-inspection service, using real
//! zip / tar.gz / gz blobs built in a temp dir.

use std::io::Write;
use std::path::PathBuf;

use bbs_rs::config::Files;
use bbs_rs::services::archive::{self, Preview};

fn cfg(max_preview_bytes: u64) -> Files {
    Files {
        max_preview_bytes,
        max_archive_entries: 1000,
        ..Files::default()
    }
}

/// A unique temp path (no external tempfile dep).
fn tmp(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("bbsrs-arch-{}-{}", std::process::id(), name));
    p
}

#[test]
fn plain_text_and_binary_detection() {
    let text = tmp("plain.txt");
    std::fs::write(&text, b"hello\nworld\n").unwrap();
    match archive::inspect(&text, "plain.txt", &cfg(1024)).unwrap() {
        Preview::Text { content, truncated } => {
            assert_eq!(content, "hello\nworld\n");
            assert!(!truncated);
        }
        _ => panic!("expected text"),
    }
    std::fs::remove_file(&text).ok();

    let bin = tmp("bin.dat");
    std::fs::write(&bin, [0u8, 1, 2, 3, 0, 255]).unwrap();
    assert!(matches!(
        archive::inspect(&bin, "bin.dat", &cfg(1024)).unwrap(),
        Preview::Binary
    ));
    std::fs::remove_file(&bin).ok();
}

#[test]
fn text_preview_is_capped() {
    let big = tmp("big.txt");
    std::fs::write(&big, "a".repeat(5000)).unwrap();
    match archive::inspect(&big, "big.txt", &cfg(100)).unwrap() {
        Preview::Text { content, truncated } => {
            assert_eq!(content.len(), 100);
            assert!(truncated);
        }
        _ => panic!("expected truncated text"),
    }
    std::fs::remove_file(&big).ok();
}

#[test]
fn gzip_single_stream_decodes() {
    use flate2::Compression;
    use flate2::write::GzEncoder;
    let gz = tmp("notes.txt.gz");
    let mut enc = GzEncoder::new(Vec::new(), Compression::default());
    enc.write_all(b"compressed text content").unwrap();
    std::fs::write(&gz, enc.finish().unwrap()).unwrap();
    match archive::inspect(&gz, "notes.txt.gz", &cfg(1024)).unwrap() {
        Preview::Text { content, .. } => assert_eq!(content, "compressed text content"),
        _ => panic!("expected text from .gz"),
    }
    std::fs::remove_file(&gz).ok();
}

#[test]
fn zip_lists_and_reads_entries() {
    let path = tmp("bundle.zip");
    {
        let file = std::fs::File::create(&path).unwrap();
        let mut zw = zip::ZipWriter::new(file);
        let opts: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
        zw.start_file("readme.txt", opts).unwrap();
        zw.write_all(b"inside the zip").unwrap();
        zw.start_file("data.bin", opts).unwrap();
        zw.write_all(&[0u8, 1, 2, 3]).unwrap();
        zw.finish().unwrap();
    }
    assert!(archive::is_archive("bundle.zip"));

    let entries = match archive::inspect(&path, "bundle.zip", &cfg(1024)).unwrap() {
        Preview::Archive { entries, truncated } => {
            assert!(!truncated);
            entries
        }
        _ => panic!("expected archive listing"),
    };
    // Entries are listed alphanumerically (added readme.txt then data.bin).
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, vec!["data.bin", "readme.txt"]);

    // A text entry decodes; a binary entry is flagged.
    match archive::read_entry(&path, "bundle.zip", "readme.txt", &cfg(1024)).unwrap() {
        Preview::Text { content, .. } => assert_eq!(content, "inside the zip"),
        _ => panic!("expected text entry"),
    }
    assert!(matches!(
        archive::read_entry(&path, "bundle.zip", "data.bin", &cfg(1024)).unwrap(),
        Preview::Binary
    ));
    // Unknown entry → NotFound.
    assert!(archive::read_entry(&path, "bundle.zip", "nope", &cfg(1024)).is_err());
    std::fs::remove_file(&path).ok();
}

#[test]
fn tar_gz_lists_and_reads_entries() {
    use flate2::Compression;
    use flate2::write::GzEncoder;
    let path = tmp("bundle.tar.gz");
    {
        let gz = GzEncoder::new(
            std::fs::File::create(&path).unwrap(),
            Compression::default(),
        );
        let mut tar = tar::Builder::new(gz);
        let body = b"tarred text";
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_cksum();
        tar.append_data(&mut header, "docs/hello.txt", &body[..])
            .unwrap();
        tar.into_inner().unwrap().finish().unwrap();
    }
    let entries = match archive::inspect(&path, "bundle.tar.gz", &cfg(1024)).unwrap() {
        Preview::Archive { entries, .. } => entries,
        _ => panic!("expected archive listing"),
    };
    assert!(entries.iter().any(|e| e.name == "docs/hello.txt"));
    match archive::read_entry(&path, "bundle.tar.gz", "docs/hello.txt", &cfg(1024)).unwrap() {
        Preview::Text { content, .. } => assert_eq!(content, "tarred text"),
        _ => panic!("expected text entry"),
    }
    std::fs::remove_file(&path).ok();
}
