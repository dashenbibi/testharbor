//! Crash-safe content-addressed evidence and bounded secret-redacted logs.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum EvidenceError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("artifact hash mismatch")]
    HashMismatch,
}

pub type Result<T> = std::result::Result<T, EvidenceError>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FinalizedArtifact {
    pub content_sha256: String,
    pub size_bytes: u64,
    pub path: PathBuf,
}

pub struct ArtifactWriter {
    root: PathBuf,
    partial_path: PathBuf,
    file: File,
    hasher: Sha256,
    size: u64,
}

impl ArtifactWriter {
    /// Creates the WRITING checkpoint. Callers persist their ArtifactIntent
    /// before constructing this writer.
    pub fn begin(root: &Path) -> Result<Self> {
        let partial_dir = root.join("partial");
        std::fs::create_dir_all(&partial_dir)?;
        let partial_path = partial_dir.join(format!("{}.partial", Uuid::new_v4()));
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&partial_path)?;
        Ok(Self {
            root: root.to_path_buf(),
            partial_path,
            file,
            hasher: Sha256::new(),
            size: 0,
        })
    }

    pub fn write_chunk(&mut self, bytes: &[u8]) -> Result<()> {
        self.file.write_all(bytes)?;
        self.hasher.update(bytes);
        self.size += bytes.len() as u64;
        Ok(())
    }

    /// Flush + fsync partial, atomically rename by content hash, then fsync
    /// the destination directory. Only after this returns may DB status move
    /// to FINALIZED.
    pub fn finalize(mut self) -> Result<FinalizedArtifact> {
        self.file.flush()?;
        self.file.sync_all()?;
        let hash = hex::encode(self.hasher.finalize());
        let object_dir = self.root.join("objects").join(&hash[..2]);
        std::fs::create_dir_all(&object_dir)?;
        let destination = object_dir.join(&hash);
        if destination.exists() {
            std::fs::remove_file(&self.partial_path)?;
        } else {
            std::fs::rename(&self.partial_path, &destination)?;
        }
        File::open(&object_dir)?.sync_all()?;
        Ok(FinalizedArtifact {
            content_sha256: hash,
            size_bytes: self.size,
            path: destination,
        })
    }
}

pub fn verify_artifact(path: &Path, expected_hash: &str) -> Result<u64> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 16 * 1024];
    let mut size = 0u64;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        size += read as u64;
    }
    if hex::encode(hasher.finalize()) != expected_hash {
        return Err(EvidenceError::HashMismatch);
    }
    Ok(size)
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ReconciliationReport {
    pub removed_partials: Vec<PathBuf>,
    pub missing: Vec<String>,
    pub corrupt: Vec<String>,
    pub orphans: Vec<PathBuf>,
}

/// Reconcile artifact files against hashes referenced by SQLite. Partials are
/// never considered finalized and are removed on startup.
pub fn reconcile(root: &Path, referenced_hashes: &HashSet<String>) -> Result<ReconciliationReport> {
    let mut report = ReconciliationReport::default();
    let partial_dir = root.join("partial");
    if partial_dir.is_dir() {
        for entry in std::fs::read_dir(&partial_dir)? {
            let path = entry?.path();
            if path.is_file() {
                std::fs::remove_file(&path)?;
                report.removed_partials.push(path);
            }
        }
    }
    for hash in referenced_hashes {
        if hash.len() < 2 {
            report.corrupt.push(hash.clone());
            continue;
        }
        let path = root.join("objects").join(&hash[..2]).join(hash);
        if !path.is_file() {
            report.missing.push(hash.clone());
        } else if verify_artifact(&path, hash).is_err() {
            report.corrupt.push(hash.clone());
        }
    }
    let objects = root.join("objects");
    if objects.is_dir() {
        for prefix in std::fs::read_dir(objects)? {
            let prefix = prefix?.path();
            if !prefix.is_dir() {
                continue;
            }
            for entry in std::fs::read_dir(prefix)? {
                let path = entry?.path();
                let hash = path
                    .file_name()
                    .and_then(|value| value.to_str())
                    .unwrap_or_default();
                if path.is_file() && !referenced_hashes.contains(hash) {
                    report.orphans.push(path);
                }
            }
        }
    }
    Ok(report)
}

pub fn redact_and_bound_log(
    text: &str,
    secrets: &[String],
    max_line_bytes: usize,
    max_total_bytes: usize,
) -> String {
    let mut output = String::new();
    for line in text.lines() {
        if output.len() >= max_total_bytes {
            break;
        }
        let mut redacted = line.to_string();
        for secret in secrets.iter().filter(|secret| !secret.is_empty()) {
            redacted = redacted.replace(secret, "[REDACTED]");
        }
        let mut end = redacted.len().min(max_line_bytes);
        while !redacted.is_char_boundary(end) {
            end -= 1;
        }
        let line = &redacted[..end];
        let remaining = max_total_bytes.saturating_sub(output.len());
        let mut take = line.len().min(remaining.saturating_sub(1));
        while !line.is_char_boundary(take) {
            take -= 1;
        }
        output.push_str(&line[..take]);
        output.push('\n');
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finalizes_and_reconciles_artifact() {
        let root = tempfile::tempdir().unwrap();
        let mut writer = ArtifactWriter::begin(root.path()).unwrap();
        writer.write_chunk(b"evidence").unwrap();
        let artifact = writer.finalize().unwrap();
        assert_eq!(
            verify_artifact(&artifact.path, &artifact.content_sha256).unwrap(),
            8
        );
        let referenced = HashSet::from([artifact.content_sha256]);
        let report = reconcile(root.path(), &referenced).unwrap();
        assert!(report.missing.is_empty());
        assert!(report.corrupt.is_empty());
    }

    #[test]
    fn redacts_before_bounding() {
        let output = redact_and_bound_log("token=secret-value", &["secret-value".into()], 128, 128);
        assert_eq!(output, "token=[REDACTED]\n");
    }
}
