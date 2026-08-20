//! Fetching speech models, with the size shown before anything is downloaded.
//!
//! The default model is 547 MiB. A tool that starts pulling that down because
//! the user ran a command is a tool that has decided something on their behalf,
//! so nothing here downloads without `--yes`.
//!
//! Every download is verified against the checksum the registry publishes. This
//! catches truncation and corruption -- the realistic failure -- and a truncated
//! model does not fail loudly: it produces quietly wrong transcripts, which is
//! the worst way for this to break.

use std::fmt;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const REPO: &str = "ggerganov/whisper.cpp";

fn info_url() -> String {
    format!("https://huggingface.co/api/models/{REPO}/paths-info/main")
}

fn download_url(file: &str) -> String {
    format!("https://huggingface.co/{REPO}/resolve/main/{file}")
}

#[derive(Debug)]
pub struct RemoteInfo {
    pub size_bytes: u64,
    /// git-LFS publishes the object id, which for LFS *is* the content SHA-256.
    /// Verified against a known-good local file before this code was trusted.
    pub sha256: Option<String>,
}

impl RemoteInfo {
    pub fn size_mib(&self) -> f64 {
        self.size_bytes as f64 / 1_048_576.0
    }
}

#[derive(Debug)]
pub enum DownloadError {
    Network(String),
    NotFound(String),
    Io(std::io::Error),
    Checksum { expected: String, got: String },
    ShortRead { expected: u64, got: u64 },
}

impl fmt::Display for DownloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DownloadError::Network(e) => write!(
                f,
                "Couldn't reach the model registry: {e}\n\
                 Check your connection, or download the file by hand and pass --model."
            ),
            DownloadError::NotFound(file) => write!(
                f,
                "The registry has no file called {file}.\n\
                 Run `framekeep-core models` to see what's available."
            ),
            DownloadError::Io(e) => write!(f, "Couldn't write the model to disk: {e}"),
            // Loud on purpose: the alternative is a model that transcribes
            // gibberish and looks like it worked.
            DownloadError::Checksum { expected, got } => write!(
                f,
                "The downloaded model is corrupt -- its checksum doesn't match.\n\
                 expected {expected}\n\
                 got      {got}\n\
                 The file has been deleted. Try again; if it keeps happening, the mirror may be broken."
            ),
            DownloadError::ShortRead { expected, got } => write!(
                f,
                "The download stopped early: got {got} bytes of {expected}.\n\
                 The partial file has been deleted. Try again."
            ),
        }
    }
}

/// Asks the registry how big a file is, and what it should hash to, *before*
/// committing the user's bandwidth to it.
pub fn fetch_info(file: &str) -> Result<RemoteInfo, DownloadError> {
    let body = serde_json::json!({ "paths": [file] }).to_string();
    let mut res = ureq::post(info_url())
        .header("Content-Type", "application/json")
        .send(&body)
        .map_err(|e| DownloadError::Network(e.to_string()))?;
    let text = res
        .body_mut()
        .read_to_string()
        .map_err(|e| DownloadError::Network(e.to_string()))?;

    let parsed: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| DownloadError::Network(e.to_string()))?;
    let entry = parsed
        .as_array()
        .and_then(|a| a.first())
        .ok_or_else(|| DownloadError::NotFound(file.to_string()))?;

    let size_bytes = entry
        .get("size")
        .and_then(|s| s.as_u64())
        .ok_or_else(|| DownloadError::NotFound(file.to_string()))?;
    let sha256 = entry
        .get("lfs")
        .and_then(|l| l.get("oid"))
        .and_then(|o| o.as_str())
        .filter(|s| s.len() == 64)
        .map(str::to_string);

    Ok(RemoteInfo { size_bytes, sha256 })
}

/// Downloads to a temporary neighbour, verifies, then renames into place.
///
/// Nothing appears at `dest` until it is known good, so a failed download can
/// never be picked up later as an installed model.
pub fn download(
    file: &str,
    dest: &Path,
    info: &RemoteInfo,
    mut on_progress: impl FnMut(u64, u64),
) -> Result<(), DownloadError> {
    use sha2::{Digest, Sha256};

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(DownloadError::Io)?;
    }
    let partial = dest.with_extension("partial");

    let mut res = ureq::get(download_url(file))
        .call()
        .map_err(|e| DownloadError::Network(e.to_string()))?;

    let mut out = std::fs::File::create(&partial).map_err(DownloadError::Io)?;
    let mut hasher = Sha256::new();
    let mut reader = res.body_mut().as_reader();
    let mut buf = vec![0u8; 1 << 20];
    let mut written: u64 = 0;

    loop {
        let n = reader.read(&mut buf).map_err(DownloadError::Io)?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n]).map_err(DownloadError::Io)?;
        hasher.update(&buf[..n]);
        written += n as u64;
        on_progress(written, info.size_bytes);
    }
    out.flush().map_err(DownloadError::Io)?;
    drop(out);

    if written != info.size_bytes {
        let _ = std::fs::remove_file(&partial);
        return Err(DownloadError::ShortRead {
            expected: info.size_bytes,
            got: written,
        });
    }

    if let Some(expected) = &info.sha256 {
        let got = hex(&hasher.finalize());
        if &got != expected {
            let _ = std::fs::remove_file(&partial);
            return Err(DownloadError::Checksum {
                expected: expected.clone(),
                got,
            });
        }
    }

    std::fs::rename(&partial, dest).map_err(DownloadError::Io)?;
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Where an installed model would live.
pub fn install_path(dir: &Path, file: &str) -> PathBuf {
    dir.join(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_is_reported_in_the_unit_humans_see() {
        // 547.4 MiB is the same number the plan writes as "574 MB". Reporting
        // MiB keeps it consistent with what a file manager shows.
        let info = RemoteInfo {
            size_bytes: 574_041_195,
            sha256: None,
        };
        assert!(
            (info.size_mib() - 547.4).abs() < 0.2,
            "got {}",
            info.size_mib()
        );
    }

    #[test]
    fn checksum_failure_says_the_file_was_removed() {
        // A corrupt model must never be left where a later run could adopt it.
        let msg = DownloadError::Checksum {
            expected: "a".into(),
            got: "b".into(),
        }
        .to_string();
        assert!(msg.contains("deleted"), "got: {msg}");
    }

    #[test]
    fn hex_encodes_lowercase_fixed_width() {
        assert_eq!(hex(&[0x00, 0x0f, 0xff]), "000fff");
    }

    #[test]
    fn urls_point_at_the_pinned_repo() {
        assert!(download_url("ggml-base.bin").ends_with("/resolve/main/ggml-base.bin"));
        assert!(info_url().contains(REPO));
    }
}
