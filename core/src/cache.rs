//! Where extracted frames live: `~/.framekeep/cache/<handle>/`.
//!
//! Outside AppData on purpose. The S0.1 spike confirmed on real hardware that
//! MSIX does not virtualize this path, so a packaged tray and an unpackaged MCP
//! adapter see the same files -- see `docs/experiments/s0-msix-named-pipe-result.md`.

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// How long derived work survives after it was last written.
///
/// Seven days, the same number the app's settings show. The rule is not the
/// same rule, though, and the difference is worth knowing: the queue counts
/// from when a recording arrived, while this counts from when these files were
/// last written. They differ because the queue is about *remembering that you
/// recorded something*, and this is about *how long a rebuildable copy sits
/// around*. Reading frames back out does not touch their timestamps, so
/// re-opening an old video does not renew it -- it pays for the extraction
/// again, which is the honest trade.
///
/// Everything under here can be rebuilt from a file the user still has,
/// offline. That is the whole reason it is allowed to be deleted without
/// asking, unlike the recordings themselves.
pub const KEEP_FOR: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Stable per-video identifier.
///
/// Derived from the absolute path plus size plus modification time, so editing
/// or re-recording a file to the same name produces a different handle and
/// cannot serve stale frames. Not a security boundary and not a content hash --
/// it only has to be stable and collision-shy across one user's files.
pub fn handle_for(path: &Path) -> std::io::Result<String> {
    let abs = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let meta = std::fs::metadata(path)?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos());
    Ok(fingerprint(&abs.to_string_lossy(), meta.len(), mtime))
}

/// The hash itself, with the filesystem taken out of it.
///
/// **`framekeep-tray` computes this too**, in `tray/src-tauri/src/handle.rs`,
/// because the queue row and the cache folder have to name the same thing. Two
/// implementations can drift, and the drift would be silent -- a row pointing
/// at a folder that nothing writes to. Both sides pin the same known answer in
/// `the_hash_is_pinned_so_the_tray_cannot_drift`; changing the algorithm here
/// fails that test in both crates, which is the point.
pub fn fingerprint(absolute_path: &str, len: u64, mtime_nanos: Option<u128>) -> String {
    let mut h = Fnv::new();
    h.write(absolute_path.as_bytes());
    h.write(&len.to_le_bytes());
    if let Some(nanos) = mtime_nanos {
        h.write(&nanos.to_le_bytes());
    }
    format!("{:016x}", h.finish())
}

pub fn root() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(|home| PathBuf::from(home).join(".framekeep").join("cache"))
}

#[derive(Debug)]
pub struct NoHome;

impl fmt::Display for NoHome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Couldn't work out where your home folder is, so there's nowhere to put frames.\n\
             Pass an output folder explicitly with --out."
        )
    }
}

pub fn dir_for(path: &Path) -> Result<PathBuf, NoHome> {
    let handle = handle_for(path).map_err(|_| NoHome)?;
    root().map(|r| r.join(handle)).ok_or(NoHome)
}

/// What one sweep did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Swept {
    pub removed: usize,
    /// Folders that refused to go -- a file still open, usually. Counted rather
    /// than ignored, because a sweep that silently does nothing looks exactly
    /// like a sweep with nothing to do.
    pub failed: usize,
}

/// Delete cache folders nothing has written to in `keep_for`.
///
/// This is the half of the retention policy that has to live in `core`. The
/// tray expires entries by walking its queue, and that works right up until
/// there is no tray: someone who installed only the MCP server has no queue,
/// no rows, and -- before this existed -- frames and transcripts that stayed
/// forever. `core` is the one process that runs in both modes.
///
/// Age is taken from the newest file in a folder, so a transcription running
/// right now is safe: it holds a lease file it wrote seconds ago.
pub fn sweep(root: &Path, keep_for: Duration, now: SystemTime) -> Swept {
    let mut swept = Swept::default();
    let Ok(entries) = std::fs::read_dir(root) else {
        return swept;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(touched) = newest_write(&path) else {
            continue;
        };
        let Ok(age) = now.duration_since(touched) else {
            // Written in the future: a clock change, not an old folder.
            continue;
        };
        if age < keep_for {
            continue;
        }
        match std::fs::remove_dir_all(&path) {
            Ok(()) => swept.removed += 1,
            Err(_) => swept.failed += 1,
        }
    }
    swept
}

/// Sweep the real cache. `None` when there is no home folder to sweep in.
pub fn sweep_default() -> Option<Swept> {
    let root = root()?;
    Some(sweep(&root, KEEP_FOR, SystemTime::now()))
}

/// The most recent write anywhere in a folder, including the folder itself so
/// an empty one still has an age.
fn newest_write(dir: &Path) -> Option<SystemTime> {
    let mut newest = std::fs::metadata(dir).and_then(|m| m.modified()).ok()?;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            if let Ok(modified) = entry.metadata().and_then(|m| m.modified()) {
                if modified > newest {
                    newest = modified;
                }
            }
        }
    }
    Some(newest)
}

/// FNV-1a, 64-bit. Small, dependency-free, and adequate for a cache key.
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
    use std::io::Write;

    /// The contract between this crate and the tray. If this number changes,
    /// the tray's copy has to change with it -- and its identical test will
    /// fail until someone does, which is the only thing standing between a
    /// queue row and a cache folder that no longer refer to the same video.
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
        // A file with no readable timestamp still gets a stable handle.
        assert_eq!(fingerprint("/home/a/x.mp4", 1, None), "4bafa0e57b15220a");
    }

    #[test]
    fn handle_changes_when_the_file_changes() {
        let mut path = std::env::temp_dir();
        path.push("framekeep-handle-test.bin");

        std::fs::write(&path, b"one").unwrap();
        let first = handle_for(&path).unwrap();

        // Same name, different content: a re-recording must not serve the old
        // frames back.
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"a different length entirely").unwrap();
        drop(f);
        let second = handle_for(&path).unwrap();

        assert_ne!(first, second);
        assert_eq!(first.len(), 16);
        let _ = std::fs::remove_file(&path);
    }

    fn sweep_fixture(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("framekeep-sweep-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    /// Make a folder look untouched for `days`, by moving every timestamp in it
    /// back rather than by waiting.
    ///
    /// The folder's own timestamp has to move too, or an empty folder can never
    /// be old and the first version of these tests passes for the wrong reason.
    fn age(dir: &Path, days: u64) {
        let when = std::fs::FileTimes::new()
            .set_modified(SystemTime::now() - Duration::from_secs(days * 24 * 60 * 60));
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                if let Ok(f) = std::fs::File::options().write(true).open(entry.path()) {
                    let _ = f.set_times(when);
                }
            }
        }
        if let Ok(f) = open_dir(dir) {
            f.set_times(when).expect("stamping the folder itself");
        }
    }

    /// Opening a directory as a file needs a flag on Windows and nothing on
    /// Unix. Without it the handle simply fails to open, quietly, and the
    /// timestamp never moves.
    #[cfg(windows)]
    fn open_dir(dir: &Path) -> std::io::Result<std::fs::File> {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(dir)
    }

    #[cfg(not(windows))]
    fn open_dir(dir: &Path) -> std::io::Result<std::fs::File> {
        std::fs::File::open(dir)
    }

    #[test]
    fn folders_nothing_has_written_to_in_a_week_are_removed() {
        let root = sweep_fixture("old");
        let old = root.join("aaaa");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("frame-00001.webp"), b"x").unwrap();
        std::fs::write(old.join("transcript.json"), b"{}").unwrap();
        age(&old, 8);

        let fresh = root.join("bbbb");
        std::fs::create_dir_all(&fresh).unwrap();
        std::fs::write(fresh.join("frame-00001.webp"), b"x").unwrap();

        let swept = sweep(&root, KEEP_FOR, SystemTime::now());
        assert_eq!(
            swept,
            Swept {
                removed: 1,
                failed: 0
            }
        );
        assert!(!old.exists(), "the old folder survived");
        assert!(fresh.exists(), "a folder written to today was deleted");
    }

    /// A transcription in flight writes a lease and then says nothing for two
    /// minutes. Its folder must not be swept out from under it.
    #[test]
    fn one_recent_file_keeps_a_folder_of_old_ones() {
        let root = sweep_fixture("lease");
        let busy = root.join("cccc");
        std::fs::create_dir_all(&busy).unwrap();
        std::fs::write(busy.join("frame-00001.webp"), b"x").unwrap();
        age(&busy, 30);
        // Written now, after the ageing: this is the lease.
        std::fs::write(busy.join("transcript.running"), b"{}").unwrap();

        assert_eq!(sweep(&root, KEEP_FOR, SystemTime::now()).removed, 0);
        assert!(busy.exists());
    }

    #[test]
    fn empty_folders_age_out_like_any_other() {
        let root = sweep_fixture("empty");
        let litter = root.join("dddd");
        std::fs::create_dir_all(&litter).unwrap();
        age(&litter, 8);

        assert_eq!(sweep(&root, KEEP_FOR, SystemTime::now()).removed, 1);
        assert!(!litter.exists());
    }

    #[test]
    fn a_cache_that_does_not_exist_yet_is_not_an_error() {
        let missing = std::env::temp_dir().join("framekeep-sweep-never-created");
        let _ = std::fs::remove_dir_all(&missing);
        assert_eq!(
            sweep(&missing, KEEP_FOR, SystemTime::now()),
            Swept::default()
        );
    }

    /// A clock that jumped backwards must not read as "everything is ancient".
    #[test]
    fn a_folder_stamped_in_the_future_is_left_alone() {
        let root = sweep_fixture("future");
        let ahead = root.join("eeee");
        std::fs::create_dir_all(&ahead).unwrap();
        std::fs::write(ahead.join("frame-00001.webp"), b"x").unwrap();

        let now = SystemTime::now() - Duration::from_secs(60 * 60 * 24 * 365);
        assert_eq!(sweep(&root, KEEP_FOR, now).removed, 0);
        assert!(ahead.exists());
    }

    #[test]
    fn handle_is_stable_for_an_unchanged_file() {
        let mut path = std::env::temp_dir();
        path.push("framekeep-handle-stable.bin");
        std::fs::write(&path, b"unchanged").unwrap();
        assert_eq!(handle_for(&path).unwrap(), handle_for(&path).unwrap());
        let _ = std::fs::remove_file(&path);
    }
}
