//! Watching one folder for new recordings. S4.7 -- optional, and off unless
//! someone turned it on.
//!
//! # The hard part is not noticing, it is waiting
//!
//! A screen recorder creates the file when it starts and fills it while you
//! work. Notice the file and import it immediately and two things break, one
//! loudly and one quietly:
//!
//!   - loudly: `core` reports `moov atom not found`, its own words for "this
//!     video is still being recorded"
//!   - quietly: the handle is a hash of path + size + **mtime**, so a file
//!     being written has a *different handle every second*. Importing on sight
//!     would add a new queue row per poll, each pointing at a cache folder
//!     nothing will ever finish filling
//!
//! So a file is only handed over once it has stopped changing. That is the
//! whole design, and it is why this polls rather than subscribing to change
//! notifications: the question is never "did something happen" but "has
//! anything happened *lately*", and a periodic size check answers that
//! directly. It also costs no dependency -- `notify` would pull a tree of
//! them to deliver events this then has to debounce anyway.
//!
//! # What it will not do
//!
//! It watches exactly one folder, not a tree, and only a folder the user
//! picked. Nothing here searches for recordings, and nothing here reads the
//! clipboard -- that is `clipboard.rs`, behind a gesture, and this file must
//! never grow a second route to it.

use crate::settings::Watch;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

/// How often the folder is listed. Slow on purpose: a recording takes minutes
/// and nobody is waiting on the first second of it.
pub const POLL: Duration = Duration::from_secs(3);

/// A file must look identical across this many polls before it is handed over.
/// Two means "unchanged for at least one full interval" -- enough for a file
/// that was copied in, and never enough for one still being written.
const STABLE_POLLS: u32 = 2;

/// What one file looked like last time we looked.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Seen {
    len: u64,
    modified: Option<SystemTime>,
    /// Consecutive polls with these exact numbers.
    still: u32,
}

/// Decides which files in a folder are ready to import.
///
/// No I/O of its own beyond listing, and no knowledge of the queue: it takes
/// what it saw and returns what has settled. That is what makes the waiting
/// rule testable without recording anything.
pub struct Folder {
    watch: Watch,
    seen: HashMap<PathBuf, Seen>,
    /// Handed over already. Kept so a file that is later touched again -- an
    /// editor rewriting it, a sync client -- is not imported twice in a row.
    delivered: HashMap<PathBuf, Seen>,
}

impl Folder {
    pub fn new(watch: Watch) -> Folder {
        Folder {
            watch,
            seen: HashMap::new(),
            delivered: HashMap::new(),
        }
    }

    /// One pass. Returns the recordings that have stopped changing since the
    /// last pass and are new enough to care about.
    pub fn settled(&mut self) -> Vec<PathBuf> {
        let mut ready = Vec::new();
        let Ok(entries) = std::fs::read_dir(&self.watch.folder) else {
            // The folder was renamed, unplugged, or never existed. Not an
            // error worth shouting about every three seconds; the next pass
            // will find it if it comes back.
            return ready;
        };

        let mut present = HashMap::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if !crate::paste::is_video(&path) {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            if !meta.is_file() {
                continue;
            }

            // Older than the moment watching was switched on. Pointing at a
            // folder of old recordings must not import two hundred of them.
            let modified = meta.modified().ok();
            if let Some(m) = modified {
                if let Ok(age) = m.duration_since(SystemTime::UNIX_EPOCH) {
                    if (age.as_secs() as i64) < self.watch.since {
                        continue;
                    }
                }
            }

            let now = Seen {
                len: meta.len(),
                modified,
                still: 0,
            };
            let previous = self.seen.get(&path);
            let still = match previous {
                Some(p) if p.len == now.len && p.modified == now.modified => p.still + 1,
                _ => 1,
            };
            let current = Seen { still, ..now };

            let already = self
                .delivered
                .get(&path)
                .is_some_and(|d| d.len == current.len && d.modified == current.modified);

            if still >= STABLE_POLLS && !already {
                ready.push(path.clone());
                self.delivered.insert(path.clone(), current.clone());
            }
            present.insert(path, current);
        }

        // Files that left the folder leave this map with them -- otherwise a
        // long-running app slowly remembers every recording ever made here.
        self.seen = present;
        self.delivered.retain(|p, _| self.seen.contains_key(p));
        ready
    }
}

/// A running watcher. Dropping the handle stops the thread at its next poll.
pub struct Handle {
    stop: Arc<AtomicBool>,
}

impl Handle {
    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Watch `watch.folder` until the handle is dropped, handing each settled
/// recording to `import`.
///
/// `import` is given a path and nothing else. It is the caller who knows what
/// a queue is -- keeping that out of here is what lets the waiting rule be
/// tested without one.
pub fn start(watch: Watch, import: impl Fn(&Path) + Send + 'static) -> Handle {
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();

    std::thread::spawn(move || {
        let mut folder = Folder::new(watch);
        while !flag.load(Ordering::SeqCst) {
            for path in folder.settled() {
                import(&path);
            }
            // Slept in slices so stopping does not wait out a whole interval.
            for _ in 0..POLL.as_secs() {
                if flag.load(Ordering::SeqCst) {
                    return;
                }
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    });

    Handle { stop }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;

    static N: AtomicU32 = AtomicU32::new(0);

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "framekeep-watch-{}-{}-{name}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn folder_watching(dir: &Path) -> Folder {
        Folder::new(Watch {
            folder: dir.to_path_buf(),
            // Everything in these tests counts as new.
            since: 0,
        })
    }

    fn write(path: &Path, bytes: usize) {
        std::fs::write(path, vec![b'x'; bytes]).unwrap();
    }

    /// The rule the whole file exists for.
    #[test]
    fn a_recording_still_being_written_is_not_handed_over() {
        let dir = temp("growing");
        let file = dir.join("screen.mp4");
        let mut folder = folder_watching(&dir);

        write(&file, 1_000);
        assert!(folder.settled().is_empty(), "handed over on first sight");

        // Still recording: the file grows between passes.
        write(&file, 5_000);
        assert!(folder.settled().is_empty(), "handed over while growing");
        write(&file, 9_000);
        assert!(folder.settled().is_empty(), "handed over while growing");

        // Recorder stopped. One unchanged pass, then it is ours.
        assert_eq!(folder.settled(), vec![file.clone()]);
    }

    #[test]
    fn a_settled_recording_is_handed_over_exactly_once() {
        let dir = temp("once");
        let file = dir.join("a.mp4");
        write(&file, 100);
        let mut folder = folder_watching(&dir);

        assert!(folder.settled().is_empty());
        assert_eq!(folder.settled(), vec![file.clone()]);
        assert!(folder.settled().is_empty(), "handed over twice");
        assert!(folder.settled().is_empty(), "handed over again");
    }

    /// Turning watching on must not import the folder's history.
    #[test]
    fn recordings_older_than_the_switch_are_left_alone() {
        let dir = temp("since");
        let old = dir.join("last-month.mp4");
        write(&old, 100);

        let future = crate::queue::now_unix() + 3600;
        let mut folder = Folder::new(Watch {
            folder: dir.clone(),
            since: future,
        });

        folder.settled();
        assert!(folder.settled().is_empty(), "an old recording was imported");
    }

    #[test]
    fn only_videos_are_picked_up() {
        let dir = temp("kinds");
        for name in ["notes.txt", "shot.png", "archive.zip", "clip.mp4"] {
            write(&dir.join(name), 50);
        }
        let mut folder = folder_watching(&dir);

        folder.settled();
        let ready = folder.settled();
        assert_eq!(ready, vec![dir.join("clip.mp4")]);
    }

    #[test]
    fn a_folder_that_is_not_there_is_quiet_not_fatal() {
        let mut folder = Folder::new(Watch {
            folder: PathBuf::from(r"C:\definitely\not\here"),
            since: 0,
        });
        assert!(folder.settled().is_empty());
        assert!(folder.settled().is_empty());
    }

    /// A removed file must not be remembered forever, or a long-lived app
    /// grows a map of every recording the folder ever held.
    #[test]
    fn files_that_leave_are_forgotten() {
        let dir = temp("forget");
        let file = dir.join("a.mp4");
        write(&file, 10);
        let mut folder = folder_watching(&dir);
        folder.settled();
        folder.settled();
        assert_eq!(folder.delivered.len(), 1);

        std::fs::remove_file(&file).unwrap();
        folder.settled();
        assert!(folder.seen.is_empty(), "a deleted file is still remembered");
        assert!(
            folder.delivered.is_empty(),
            "a deleted file is still remembered"
        );
    }

    #[test]
    fn stopping_the_thread_actually_stops_it() {
        let dir = temp("stop");
        let count = Arc::new(AtomicU32::new(0));
        let seen = count.clone();
        let handle = start(
            Watch {
                folder: dir.clone(),
                since: 0,
            },
            move |_| {
                seen.fetch_add(1, Ordering::SeqCst);
            },
        );
        handle.stop();
        std::thread::sleep(Duration::from_millis(200));
        drop(handle);
        assert_eq!(count.load(Ordering::SeqCst), 0);
    }
}
