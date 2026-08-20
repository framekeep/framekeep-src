//! The per-video identifier, computed exactly as `framekeep-core` computes it.
//!
//! # Why this is duplicated, and what stops it drifting
//!
//! The queue's primary key and the cache folder's name are the same string:
//! `queue.db` says `handle`, `core` writes `~/.framekeep/cache/<handle>/`. If
//! the two ever disagreed the failure would be silent -- a row pointing at a
//! folder nothing fills, frames that nobody can find from the queue.
//!
//! The alternative was a shared crate for sixteen lines of FNV, or spawning
//! `framekeep-core` on every paste to ask. Both cost more than they save. What
//! is cheap is making drift impossible to miss: [`fingerprint`] is pinned to
//! the same known answers as `core/src/cache.rs`, in a test of the same name.
//! Change the algorithm in one crate and both tests fail.

use std::path::Path;

/// `Ok` with the handle, or the reason there is no readable file there.
pub fn for_path(path: &Path) -> std::io::Result<String> {
    let absolute = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let meta = std::fs::metadata(path)?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos());
    Ok(fingerprint(&absolute.to_string_lossy(), meta.len(), mtime))
}

/// Path plus size plus modification time, hashed.
///
/// Re-recording over a file gives a different handle, so a stale set of frames
/// can never be served for new content. Not a content hash and not a security
/// boundary: it only has to be stable, and unlikely to collide across one
/// person's files.
pub fn fingerprint(absolute_path: &str, len: u64, mtime_nanos: Option<u128>) -> String {
    let mut h = Fnv::new();
    h.write(absolute_path.as_bytes());
    h.write(&len.to_le_bytes());
    if let Some(nanos) = mtime_nanos {
        h.write(&nanos.to_le_bytes());
    }
    format!("{:016x}", h.finish())
}

/// FNV-1a, 64-bit. Byte for byte what `core/src/cache.rs` runs.
struct Fnv(u64);

impl Fnv {
    fn new() -> Self {
        Fnv(0xcbf2_9ce4_8422_2325)
    }
    fn write(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.0 ^= *b as u64;
            self.0 = self.0.wrapping_mul(0x1000_0000_01b3);
        }
    }
    fn finish(&self) -> u64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The contract with `framekeep-core`. The same two numbers are asserted in
    /// `core/src/cache.rs`, in a test of the same name. They have to move
    /// together or not at all.
    #[test]
    fn the_hash_is_pinned_so_the_tray_cannot_drift() {
        assert_eq!(
            fingerprint(
                r"C:\Users\Nguyễn Văn A\Videos\test.mp4",
                12_345,
                Some(1_700_000_000_000_000_000)
            ),
            "dbcebdf3d95573e2"
        );
        assert_eq!(fingerprint("/home/a/x.mp4", 1, None), "4bafa0e57b15220a");
    }

    #[test]
    fn a_handle_is_sixteen_hex_characters() {
        let h = fingerprint("/a/b.mp4", 10, Some(1));
        assert_eq!(h.len(), 16);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn re_recording_over_a_file_produces_a_different_handle() {
        let path = r"C:\a\demo.mp4";
        let first = fingerprint(path, 1_000, Some(100));
        assert_ne!(first, fingerprint(path, 2_000, Some(100)), "size ignored");
        assert_ne!(first, fingerprint(path, 1_000, Some(200)), "mtime ignored");
    }

    #[test]
    fn a_missing_file_says_so_rather_than_hashing_nothing() {
        let missing = std::path::Path::new(r"C:\definitely\not\here.mp4");
        assert!(for_path(missing).is_err());
    }
}
