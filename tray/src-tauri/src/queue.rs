//! `~/.framekeep/queue.db` -- the work list.
//!
//! **A work list, not a journal.** Items leave when they are done with. A local
//! database that remembers every recording forever is the thing this product's
//! users are getting away from, and `AGENTS.md` already bans an analysis
//! history from the app; a table with no screen attached is still one.
//!
//! What is *not* here is the load-bearing part. No transcript, no OCR text, no
//! detected secret values. Those have one home each, under
//! `~/.framekeep/cache/<handle>/`, so deleting a row deletes everything the row
//! knew about. Two homes would mean deleting one and keeping the other, which
//! is exactly how the S2.10 injection hole worked: a fence that looked like a
//! fence because the second copy was out of sight.
//!
//! The most sensitive column is not the one people expect. It is `source_path`:
//! `C:\work\acme-bank\payment-demo.mp4` gives up the client, the project and
//! the task in one string. That is why the clock applies to the row itself and
//! not merely to the frames.

use crate::retention::{LeaveReason, Origin, Retention, SourceVerdict};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::{Path, PathBuf};

/// Every column, in order. The schema is written out rather than migrated into
/// existence because this list *is* the policy: a column that does not appear
/// here had to be argued for first.
const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS recordings (
    handle        TEXT PRIMARY KEY,
    source_path   TEXT NOT NULL,
    display_name  TEXT NOT NULL,
    origin        TEXT NOT NULL CHECK (origin IN ('referenced','copied')),
    created_at    INTEGER NOT NULL,
    status        TEXT NOT NULL CHECK (status IN
                    ('extracting_frames','transcribing','scanning','needs_review','ready','error')),
    duration_ms   INTEGER,
    width         INTEGER,
    height        INTEGER,
    frame_count   INTEGER,
    finding_count INTEGER,
    reviewed_at   INTEGER,
    error         TEXT,
    -- A failed recording with no explanation is the failure this project keeps
    -- catching in its own code. The database refuses to store one.
    CHECK (status <> 'error' OR error IS NOT NULL)
);
CREATE INDEX IF NOT EXISTS recordings_by_age ON recordings (created_at);
"#;

/// The columns any build is allowed to have. Adding one means changing this
/// list, which means a test failure that points at the policy -- see
/// `the_schema_holds_no_room_for_content`.
pub const ALLOWED_COLUMNS: [&str; 13] = [
    "handle",
    "source_path",
    "display_name",
    "origin",
    "created_at",
    "status",
    "duration_ms",
    "width",
    "height",
    "frame_count",
    "finding_count",
    "reviewed_at",
    "error",
];

/// The stage a recording is at. Shown as its own words -- `Extracting frames`,
/// `Transcribing`, `Scanning for secrets` -- never as a generic "Processing":
/// whisper runs for tens of seconds and a silent spinner is not an answer.
///
/// These are storage names. The display strings live in the copy file.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Status {
    ExtractingFrames,
    Transcribing,
    Scanning,
    NeedsReview,
    Ready,
    Error,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::ExtractingFrames => "extracting_frames",
            Status::Transcribing => "transcribing",
            Status::Scanning => "scanning",
            Status::NeedsReview => "needs_review",
            Status::Ready => "ready",
            Status::Error => "error",
        }
    }

    pub fn parse(s: &str) -> Option<Status> {
        Some(match s {
            "extracting_frames" => Status::ExtractingFrames,
            "transcribing" => Status::Transcribing,
            "scanning" => Status::Scanning,
            "needs_review" => Status::NeedsReview,
            "ready" => Status::Ready,
            "error" => Status::Error,
            _ => return None,
        })
    }
}

/// One frame a caller may read, already resolved to the file that is safe to
/// hand over. `redacted` is carried so the reply can say which it got --
/// "hidden" and "nothing was found" must never look the same downstream.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub pts_time: f64,
    pub file: PathBuf,
    pub redacted: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Recording {
    pub handle: String,
    pub source_path: PathBuf,
    pub display_name: String,
    pub origin: Origin,
    pub created_at: i64,
    pub status: Status,
    pub duration_ms: Option<i64>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub frame_count: Option<i64>,
    pub finding_count: Option<i64>,
    pub reviewed_at: Option<i64>,
    pub error: Option<String>,
}

impl Recording {
    /// A new arrival. `display_name` comes from the file name, which is also
    /// the only thing the queue's search box is allowed to match on.
    pub fn new(
        handle: &str,
        source_path: impl Into<PathBuf>,
        origin: Origin,
        created_at: i64,
    ) -> Recording {
        let source_path = source_path.into();
        let display_name = source_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| handle.to_string());
        Recording {
            handle: handle.to_string(),
            source_path,
            display_name,
            origin,
            created_at,
            status: Status::ExtractingFrames,
            duration_ms: None,
            width: None,
            height: None,
            frame_count: None,
            finding_count: None,
            reviewed_at: None,
            error: None,
        }
    }
}

#[derive(Debug)]
pub enum QueueError {
    Db(rusqlite::Error),
    /// Something on disk refused. Carries what was being touched, because
    /// "access denied" on its own tells nobody what to do next.
    Io(std::io::Error, String),
    /// A row that cannot be read back as itself.
    Corrupt(String),
}

impl std::fmt::Display for QueueError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            QueueError::Db(e) => write!(
                f,
                "Framekeep could not read its own queue ({e}). \
                 Close and reopen the app; if it keeps happening, report it."
            ),
            QueueError::Io(e, what) => write!(
                f,
                "Couldn't remove {what} ({e}). It may still be open somewhere."
            ),
            QueueError::Corrupt(what) => write!(
                f,
                "A queue entry is unreadable ({what}). Framekeep left it alone rather than guessing."
            ),
        }
    }
}

impl std::error::Error for QueueError {}

impl From<rusqlite::Error> for QueueError {
    fn from(e: rusqlite::Error) -> Self {
        QueueError::Db(e)
    }
}

pub type Result<T> = std::result::Result<T, QueueError>;

pub struct Queue {
    conn: Connection,
    cache_root: PathBuf,
}

impl Queue {
    /// The real locations: `~/.framekeep/queue.db` and `~/.framekeep/cache/`.
    ///
    /// Both are in the hidden folder because both can be rebuilt from files the
    /// user still has, offline. What cannot be rebuilt without a network -- the
    /// recordings themselves, the whisper models -- lives in `~/Framekeep/`
    /// where a person can see it.
    pub fn open() -> Result<Queue> {
        let home = std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .ok_or_else(|| {
                QueueError::Io(
                    std::io::Error::new(std::io::ErrorKind::NotFound, "no home folder"),
                    "your home folder".to_string(),
                )
            })?;
        let root = PathBuf::from(home).join(".framekeep");
        Queue::open_at(root.join("queue.db"), root.join("cache"))
    }

    pub fn open_at(db_path: impl AsRef<Path>, cache_root: impl Into<PathBuf>) -> Result<Queue> {
        let db_path = db_path.as_ref();
        if let Some(dir) = db_path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| QueueError::Io(e, dir.display().to_string()))?;
        }
        let conn = Connection::open(db_path)?;

        // Two clients open at once is the normal case here -- Cursor and Claude
        // Code, with the tray writing underneath both. WAL lets them read while
        // a write is in flight; the timeout means a writer waits its turn
        // instead of handing back "database is locked" as if it were an answer.
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA busy_timeout=5000;",
        )?;
        conn.execute_batch(SCHEMA)?;

        Ok(Queue {
            conn,
            cache_root: cache_root.into(),
        })
    }

    pub fn cache_dir(&self, handle: &str) -> PathBuf {
        self.cache_root.join(handle)
    }

    /// The first frame core extracted, if any -- the queue screen's thumbnail.
    ///
    /// Read from `frames.json`, the index core writes next to the frames. The
    /// file's absence is normal (extraction still running, or swept), so this
    /// is an `Option`, never an error. The path is verified to still exist:
    /// a thumbnail that 404s inside the window looks like a broken product.
    pub fn first_frame(&self, handle: &str) -> Option<PathBuf> {
        self.frames(handle).into_iter().next().map(|f| f.file)
    }

    /// Every frame of a recording, with the redacted copy standing in wherever
    /// one exists.
    ///
    /// **This substitution is the product's promise made mechanical.** A
    /// recording that went through the app and was approved must never hand a
    /// model the original pixels of a frame a person had masked, and the way to
    /// guarantee that is to make the masked file the ONLY one this door can
    /// name. Callers get paths and never choose between two of them.
    ///
    /// Frames whose file is gone are dropped rather than listed: a path to
    /// nothing turns a missing picture into a confusing error further away.
    /// That is right for one frame and a lie for all of them, which is what
    /// [`Queue::frames_checked`] exists to tell apart.
    pub fn frames(&self, handle: &str) -> Vec<Frame> {
        self.frames_checked(handle).unwrap_or_default()
    }

    /// The same list, plus the one thing the list itself cannot say: whether
    /// this cache is intact.
    ///
    /// `Err` means the index named frames and **not one of them is there**.
    /// Dropping missing frames silently is fine at one-in-twenty and dishonest
    /// at twenty-in-twenty: the caller cannot tell a relocated cache from a
    /// short recording, and it will report the second.
    ///
    /// Measured, not imagined. Moving `~/.visionai` to `~/.framekeep` left the
    /// absolute paths in `frames.json` stale, and over the real pipe a
    /// twenty-frame recording answered with one and no error anywhere. Core
    /// now writes those paths relative to this folder so it cannot happen
    /// again; this is the check for the caches that already exist, and for the
    /// next thing nobody predicted.
    // Spelled out because this module's own `Result` alias takes one parameter
    // and always means `QueueError`. A broken cache is not a database fault.
    pub fn frames_checked(&self, handle: &str) -> std::result::Result<Vec<Frame>, String> {
        let cache = self.cache_dir(handle);
        let Ok(index) = std::fs::read_to_string(cache.join("frames.json")) else {
            return Ok(Vec::new());
        };
        let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&index) else {
            return Ok(Vec::new());
        };
        let Some(list) = parsed.get("frames").and_then(|f| f.as_array()) else {
            return Ok(Vec::new());
        };

        let found: Vec<Frame> = list
            .iter()
            .filter_map(|f| {
                // Joined against the cache folder: a relative name lands in
                // it, and an absolute one from an older index replaces it, so
                // both spellings read the same way with nothing to detect.
                let original = cache.join(f.get("file")?.as_str()?);
                let pts_time = f.get("pts_time")?.as_f64()?;
                // redacted/<stem>.webp -- the name `review::apply` writes.
                let masked = original
                    .file_stem()
                    .map(|s| {
                        cache
                            .join("redacted")
                            .join(format!("{}.webp", s.to_string_lossy()))
                    })
                    .filter(|p| p.is_file());
                let redacted = masked.is_some();
                let file = masked.or_else(|| original.is_file().then_some(original))?;
                Some(Frame {
                    pts_time,
                    file,
                    redacted,
                })
            })
            .collect();

        if found.is_empty() && !list.is_empty() {
            return Err(format!(
                "Framekeep has an index of {} frames for this recording but none of the \
                 files are there. The cache folder may have been moved or cleaned out. \
                 Looked in {}.",
                list.len(),
                cache.display()
            ));
        }
        Ok(found)
    }

    /// Add a recording, or update one that is already here.
    ///
    /// `created_at` is written once and never again. Re-opening a video a week
    /// later must not buy it another week -- that is L3, and it is enforced
    /// here rather than trusted to callers.
    pub fn put(&self, r: &Recording) -> Result<()> {
        self.conn.execute(
            "INSERT INTO recordings
               (handle, source_path, display_name, origin, created_at, status,
                duration_ms, width, height, frame_count, finding_count, reviewed_at, error)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)
             ON CONFLICT(handle) DO UPDATE SET
               source_path   = excluded.source_path,
               display_name  = excluded.display_name,
               origin        = excluded.origin,
               status        = excluded.status,
               duration_ms   = excluded.duration_ms,
               width         = excluded.width,
               height        = excluded.height,
               frame_count   = excluded.frame_count,
               finding_count = excluded.finding_count,
               reviewed_at   = excluded.reviewed_at,
               error         = excluded.error",
            params![
                r.handle,
                r.source_path.to_string_lossy(),
                r.display_name,
                r.origin.as_str(),
                r.created_at,
                r.status.as_str(),
                r.duration_ms,
                r.width,
                r.height,
                r.frame_count,
                r.finding_count,
                r.reviewed_at,
                r.error,
            ],
        )?;
        Ok(())
    }

    pub fn get(&self, handle: &str) -> Result<Option<Recording>> {
        self.conn
            .query_row(
                &format!(
                    "SELECT {} FROM recordings WHERE handle = ?1",
                    ALLOWED_COLUMNS.join(",")
                ),
                params![handle],
                row_to_recording,
            )
            .optional()?
            .transpose()
    }

    /// Newest first, which is the order the queue screen shows.
    pub fn list(&self, limit: usize) -> Result<Vec<Recording>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {} FROM recordings ORDER BY created_at DESC LIMIT ?1",
            ALLOWED_COLUMNS.join(",")
        ))?;
        let rows = stmt.query_map(params![limit as i64], row_to_recording)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row??);
        }
        Ok(out)
    }

    /// L4: the only way anything is deleted.
    ///
    /// Order matters and is not arbitrary. Files go first, the row goes last,
    /// so an interrupted purge leaves a row pointing at things that are already
    /// gone -- which the next run finishes cleanly. The other order would leave
    /// frames with no row, and nothing would ever look at them again.
    ///
    /// If any step fails, the row stays and the error is returned. Retrying
    /// later beats orphaning content now.
    pub fn purge(&self, handle: &str, retention: &Retention) -> Result<Option<Purged>> {
        let Some(recording) = self.get(handle)? else {
            return Ok(None);
        };

        let source = match retention.source_verdict(recording.origin, &recording.source_path) {
            SourceVerdict::Leave(reason) => SourceOutcome::Left(reason),
            SourceVerdict::Delete => {
                std::fs::remove_file(&recording.source_path)
                    .map_err(|e| QueueError::Io(e, recording.source_path.display().to_string()))?;
                SourceOutcome::Deleted
            }
        };

        let cache = self.cache_dir(handle);
        let cache_removed = match std::fs::remove_dir_all(&cache) {
            Ok(()) => true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => return Err(QueueError::Io(e, cache.display().to_string())),
        };

        self.conn
            .execute("DELETE FROM recordings WHERE handle = ?1", params![handle])?;

        Ok(Some(Purged {
            handle: handle.to_string(),
            cache_removed,
            source,
        }))
    }

    /// Everything past its keep-for, purged. Status buys no exemption -- least
    /// of all `needs_review`, which by definition is the entry still holding
    /// un-redacted frames on disk.
    pub fn expire(&self, now: i64, retention: &Retention) -> Result<Vec<Purged>> {
        let mut stmt = self
            .conn
            .prepare("SELECT handle, created_at FROM recordings")?;
        let due: Vec<String> = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .filter(|(_, created_at)| retention.expired(*created_at, now))
            .map(|(handle, _)| handle)
            .collect();

        let mut purged = Vec::new();
        for handle in due {
            if let Some(p) = self.purge(&handle, retention)? {
                purged.push(p);
            }
        }
        Ok(purged)
    }

    /// Remove cache folders that hold nothing and belong to nobody.
    ///
    /// Measured on this machine on 17/08/2026 before any of this existed: 57
    /// handle folders, 33 of them empty. They cost nothing individually and
    /// never stop arriving.
    ///
    /// **Empty only.** A folder with frames in it and no queue row is the
    /// normal shape of standalone mode -- the MCP adapter driving `core` with
    /// no tray running -- and deleting those would take away a working user's
    /// results. Ageing those out is `core`'s job and is not built yet; see the
    /// debt table in `BUILD-PROGRESS.md`.
    pub fn sweep_orphans(&self) -> Result<Vec<String>> {
        let dir = match std::fs::read_dir(&self.cache_root) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(QueueError::Io(e, self.cache_root.display().to_string())),
        };

        let mut known = std::collections::HashSet::new();
        let mut stmt = self.conn.prepare("SELECT handle FROM recordings")?;
        for handle in stmt.query_map([], |row| row.get::<_, String>(0))? {
            known.insert(handle?);
        }

        let mut swept = Vec::new();
        for entry in dir.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if known.contains(&name) {
                continue;
            }
            let empty = std::fs::read_dir(&path)
                .map(|mut d| d.next().is_none())
                .unwrap_or(false);
            if empty && std::fs::remove_dir(&path).is_ok() {
                swept.push(name);
            }
        }
        Ok(swept)
    }
}

#[derive(Debug)]
pub struct Purged {
    pub handle: String,
    pub cache_removed: bool,
    pub source: SourceOutcome,
}

#[derive(Debug, PartialEq, Eq)]
pub enum SourceOutcome {
    Deleted,
    Left(LeaveReason),
}

/// Seconds since the epoch. One place, so a test can pass its own instead.
pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

type RowResult = rusqlite::Result<Result<Recording>>;

fn row_to_recording(row: &rusqlite::Row<'_>) -> RowResult {
    let handle: String = row.get(0)?;
    let origin_raw: String = row.get(3)?;
    let status_raw: String = row.get(5)?;

    let Some(origin) = Origin::parse(&origin_raw) else {
        return Ok(Err(QueueError::Corrupt(format!(
            "{handle}: origin {origin_raw:?}"
        ))));
    };
    let Some(status) = Status::parse(&status_raw) else {
        return Ok(Err(QueueError::Corrupt(format!(
            "{handle}: status {status_raw:?}"
        ))));
    };

    Ok(Ok(Recording {
        handle,
        source_path: PathBuf::from(row.get::<_, String>(1)?),
        display_name: row.get(2)?,
        origin,
        created_at: row.get(4)?,
        status,
        duration_ms: row.get(6)?,
        width: row.get(7)?,
        height: row.get(8)?,
        frame_count: row.get(9)?,
        finding_count: row.get(10)?,
        reviewed_at: row.get(11)?,
        error: row.get(12)?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static N: AtomicU32 = AtomicU32::new(0);

    struct Fixture {
        queue: Queue,
        root: PathBuf,
        recordings_dir: PathBuf,
    }

    impl Fixture {
        fn new(name: &str) -> Fixture {
            let root = std::env::temp_dir().join(format!(
                "framekeep-queue-{}-{}-{name}",
                std::process::id(),
                N.fetch_add(1, Ordering::SeqCst)
            ));
            let _ = std::fs::remove_dir_all(&root);
            let recordings_dir = root.join("Recordings");
            std::fs::create_dir_all(&recordings_dir).unwrap();
            let queue = Queue::open_at(root.join("queue.db"), root.join("cache")).unwrap();
            Fixture {
                queue,
                root,
                recordings_dir,
            }
        }

        fn retention(&self) -> Retention {
            Retention {
                choice_made: true,
                ..Retention::new(&self.recordings_dir)
            }
        }

        /// A recording with frames and a transcript on disk, the way a real one
        /// arrives.
        fn ingest(
            &self,
            handle: &str,
            origin: Origin,
            created_at: i64,
            transcript: &str,
        ) -> Recording {
            let source = match origin {
                Origin::Copied => self.recordings_dir.join(format!("{handle}.mp4")),
                Origin::Referenced => self.root.join(format!("theirs-{handle}.mp4")),
            };
            std::fs::write(&source, b"not really a video").unwrap();

            let cache = self.queue.cache_dir(handle);
            std::fs::create_dir_all(&cache).unwrap();
            std::fs::write(cache.join("frame-00001.webp"), b"frame").unwrap();
            std::fs::write(
                cache.join("transcript.json"),
                format!(r#"{{"segments":[{{"text":"{transcript}"}}]}}"#),
            )
            .unwrap();

            let r = Recording::new(handle, &source, origin, created_at);
            self.queue.put(&r).unwrap();
            r
        }

        fn db_bytes(&self) -> Vec<u8> {
            // WAL: the newest writes may still be in the sidecar file, so a scan
            // of queue.db alone could miss exactly what it is looking for.
            let mut all = Vec::new();
            for suffix in ["", "-wal", "-shm"] {
                let p = self.root.join(format!("queue.db{suffix}"));
                if let Ok(bytes) = std::fs::read(&p) {
                    all.extend_from_slice(&bytes);
                }
            }
            all
        }
    }

    // --- test 1 -------------------------------------------------------------

    #[test]
    fn an_expired_row_takes_its_cache_folder_with_it() {
        let f = Fixture::new("expire");
        let day = 24 * 60 * 60;
        f.ingest("old", Origin::Referenced, 0, "hello");
        f.ingest("fresh", Origin::Referenced, 6 * day, "hello");

        let purged = f.queue.expire(7 * day, &f.retention()).unwrap();
        assert_eq!(purged.len(), 1);
        assert_eq!(purged[0].handle, "old");
        assert!(purged[0].cache_removed);

        assert!(f.queue.get("old").unwrap().is_none(), "the row survived");
        assert!(!f.queue.cache_dir("old").exists(), "the frames survived");
        assert!(
            f.queue.get("fresh").unwrap().is_some(),
            "the wrong row went"
        );
        assert!(f.queue.cache_dir("fresh").exists());
    }

    // --- test 2: the heaviest one ------------------------------------------

    #[test]
    fn purging_never_deletes_a_file_the_user_pointed_at() {
        let f = Fixture::new("referenced");
        let r = f.ingest("theirs", Origin::Referenced, 0, "hello");

        let purged = f.queue.purge("theirs", &f.retention()).unwrap().unwrap();

        assert!(
            r.source_path.exists(),
            "Framekeep deleted a file it did not create: {}",
            r.source_path.display()
        );
        assert_eq!(purged.source, SourceOutcome::Left(LeaveReason::NotOurs));
        // The derived work is gone; only the user's own file is untouched.
        assert!(!f.queue.cache_dir("theirs").exists());
    }

    // --- test 3 -------------------------------------------------------------

    #[test]
    fn a_copied_file_moved_out_of_our_folder_is_left_alone() {
        let f = Fixture::new("movedout");
        let r = f.ingest("ours", Origin::Copied, 0, "hello");

        let moved = f.root.join("somewhere-else.mp4");
        std::fs::rename(&r.source_path, &moved).unwrap();
        let mut moved_row = r.clone();
        moved_row.source_path = moved.clone();
        f.queue.put(&moved_row).unwrap();

        let purged = f.queue.purge("ours", &f.retention()).unwrap().unwrap();
        assert!(
            moved.exists(),
            "a file the user moved out was still deleted"
        );
        assert_eq!(purged.source, SourceOutcome::Left(LeaveReason::MovedOut));
    }

    #[test]
    fn a_copied_file_still_in_our_folder_is_deleted_once_agreed() {
        let f = Fixture::new("copied");
        let r = f.ingest("ours", Origin::Copied, 0, "hello");
        let purged = f.queue.purge("ours", &f.retention()).unwrap().unwrap();
        assert_eq!(purged.source, SourceOutcome::Deleted);
        assert!(!r.source_path.exists());
    }

    // --- test 4 -------------------------------------------------------------

    #[test]
    fn purging_leaves_no_orphan_folder_behind() {
        let f = Fixture::new("orphan");
        f.ingest("a", Origin::Referenced, 0, "hello");
        f.queue.purge("a", &f.retention()).unwrap();

        let left: Vec<_> = std::fs::read_dir(f.root.join("cache"))
            .map(|d| d.flatten().map(|e| e.file_name()).collect())
            .unwrap_or_default();
        assert!(left.is_empty(), "cache still holds {left:?}");
    }

    // --- test 5 -------------------------------------------------------------

    #[test]
    fn empty_folders_nobody_owns_are_swept_and_useful_ones_are_not() {
        let f = Fixture::new("sweep");
        let cache = f.root.join("cache");
        std::fs::create_dir_all(cache.join("litter")).unwrap();

        // Standalone mode: frames, no queue row. Must survive.
        std::fs::create_dir_all(cache.join("standalone")).unwrap();
        std::fs::write(cache.join("standalone").join("frame-00001.webp"), b"x").unwrap();

        // Queued but nothing extracted yet. Must survive.
        f.queue
            .put(&Recording::new("queued", "C:/x.mp4", Origin::Referenced, 0))
            .unwrap();
        std::fs::create_dir_all(cache.join("queued")).unwrap();

        let swept = f.queue.sweep_orphans().unwrap();
        assert_eq!(swept, vec!["litter".to_string()]);
        assert!(
            cache.join("standalone").exists(),
            "standalone results were deleted"
        );
        assert!(
            cache.join("queued").exists(),
            "a queued entry's folder was deleted"
        );
    }

    // --- the cache has to survive being moved ------------------------------

    /// Write an index the way an OLD core did: absolute paths.
    fn legacy_index(cache: &std::path::Path, files: &[&str]) {
        std::fs::create_dir_all(cache).unwrap();
        let entries: Vec<String> = files
            .iter()
            .enumerate()
            .map(|(i, name)| {
                std::fs::write(cache.join(name), b"x").unwrap();
                format!(
                    r#"{{"pts_time":{}.0,"file":{}}}"#,
                    i,
                    serde_json::to_string(&cache.join(name).display().to_string()).unwrap()
                )
            })
            .collect();
        std::fs::write(
            cache.join("frames.json"),
            format!(r#"{{"frames":[{}]}}"#, entries.join(",")),
        )
        .unwrap();
    }

    /// An index written by an older core still reads. No migration, because
    /// joining a folder with an absolute path yields the absolute path.
    #[test]
    fn an_index_of_absolute_paths_from_an_older_build_still_reads() {
        let f = Fixture::new("legacy-abs");
        let cache = f.queue.cache_dir("a");
        legacy_index(&cache, &["frame-00001.webp", "frame-00002.webp"]);

        let frames = f.queue.frames("a");
        assert_eq!(frames.len(), 2, "{frames:?}");
        assert!(frames[0].file.is_file());
    }

    /// The relative form, which is what core writes now, and the point of it:
    /// the folder can move and the index still resolves.
    #[test]
    fn an_index_of_relative_names_follows_the_folder_when_it_moves() {
        let f = Fixture::new("relative");
        let cache = f.queue.cache_dir("a");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join("frame-00001.webp"), b"x").unwrap();
        std::fs::write(
            cache.join("frames.json"),
            r#"{"frames":[{"pts_time":0.0,"file":"frame-00001.webp"}]}"#,
        )
        .unwrap();

        assert_eq!(f.queue.frames("a").len(), 1);

        // Now be the move that broke this: same folder contents, new root.
        let moved_root = f.root.join("moved");
        std::fs::create_dir_all(&moved_root).unwrap();
        let moved = Queue::open_at(f.root.join("queue.db"), moved_root.join("cache")).unwrap();
        let moved_cache = moved.cache_dir("a");
        std::fs::create_dir_all(moved_cache.parent().unwrap()).unwrap();
        copy_dir(&cache, &moved_cache);

        let frames = moved.frames("a");
        assert_eq!(
            frames.len(),
            1,
            "a relative index stopped resolving after a move: {frames:?}"
        );
        assert!(frames[0].file.starts_with(&moved_root));
    }

    fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
        std::fs::create_dir_all(to).unwrap();
        for entry in std::fs::read_dir(from).unwrap().flatten() {
            let target = to.join(entry.file_name());
            if entry.path().is_dir() {
                copy_dir(&entry.path(), &target);
            } else {
                std::fs::copy(entry.path(), target).unwrap();
            }
        }
    }

    /// The honesty rule. One frame missing out of twenty is noise worth
    /// dropping; twenty out of twenty is a broken cache, and reporting it as a
    /// one-frame recording is the failure that shipped.
    #[test]
    fn an_index_whose_files_are_all_gone_is_reported_not_served_as_empty() {
        let f = Fixture::new("all-gone");
        let cache = f.queue.cache_dir("a");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(
            cache.join("frames.json"),
            r#"{"frames":[{"pts_time":0.0,"file":"frame-00001.webp"},
                         {"pts_time":5.0,"file":"frame-00002.webp"}]}"#,
        )
        .unwrap();

        let why = f
            .queue
            .frames_checked("a")
            .expect_err("a cache with no files at all was reported as an empty recording");
        assert!(why.contains("2 frames"), "{why}");
        assert!(why.contains("moved or cleaned out"), "{why}");
    }

    /// And the case that must NOT be an error: some gone, some there.
    #[test]
    fn losing_one_frame_of_several_stays_a_shorter_list_and_not_a_failure() {
        let f = Fixture::new("partly-gone");
        let cache = f.queue.cache_dir("a");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join("frame-00002.webp"), b"x").unwrap();
        std::fs::write(
            cache.join("frames.json"),
            r#"{"frames":[{"pts_time":0.0,"file":"frame-00001.webp"},
                         {"pts_time":5.0,"file":"frame-00002.webp"}]}"#,
        )
        .unwrap();

        let frames = f
            .queue
            .frames_checked("a")
            .expect("one missing frame is not a broken cache");
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].pts_time, 5.0);
    }

    /// No index at all is the ordinary case -- extraction has not run, or the
    /// folder was swept -- and stays an empty list rather than a failure.
    #[test]
    fn no_index_at_all_is_empty_and_not_an_error() {
        let f = Fixture::new("no-index");
        assert!(f
            .queue
            .frames_checked("never-extracted")
            .unwrap()
            .is_empty());
    }

    // --- test 6: do not ask the schema, ask the file -----------------------

    #[test]
    fn nothing_a_person_said_ever_reaches_the_database_file() {
        let f = Fixture::new("content");
        let secret = "the deployment key is in the vault under acme-prod";
        f.ingest("a", Origin::Referenced, 0, secret);

        // A full lifecycle, in case content sneaks in through an update path.
        let mut r = f.queue.get("a").unwrap().unwrap();
        r.status = Status::Ready;
        r.frame_count = Some(11);
        r.finding_count = Some(2);
        r.reviewed_at = Some(123);
        f.queue.put(&r).unwrap();

        let bytes = f.db_bytes();
        let needle = secret.as_bytes();
        assert!(
            !bytes.windows(needle.len()).any(|w| w == needle),
            "transcript text reached queue.db -- see docs/spec-s3-retention.md L1"
        );
        // The test has to be able to fail: prove the scan finds what is there.
        let name = b"a.mp4";
        assert!(
            bytes.windows(name.len()).any(|w| w == name),
            "the scan found nothing at all, so its silence about the transcript means nothing"
        );
    }

    /// The column list guards `recordings`. This guards the shape one step out,
    /// because the likeliest way content arrives is not a column -- it is a
    /// second table, arriving as "search recordings by what was said".
    #[test]
    fn there_is_exactly_one_table_and_it_is_the_work_list() {
        let f = Fixture::new("tables");
        // Through the whole lifecycle first. Checking a freshly opened database
        // would only prove that `SCHEMA` says what `SCHEMA` says; the table
        // that matters is one some later code path creates while running.
        f.ingest("a", Origin::Referenced, 0, "something said out loud");
        let mut r = f.queue.get("a").unwrap().unwrap();
        r.status = Status::Ready;
        f.queue.put(&r).unwrap();

        let mut stmt = f
            .queue
            .conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
            .unwrap();
        let tables: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .map(|t| t.unwrap())
            .collect();

        assert_eq!(
            tables,
            ["recordings"],
            "a second table appeared. A full-text index over transcripts is the \
             competitor's memory.db built by our own hand -- see \
             docs/spec-s3-retention.md L1 and the S4.6 debt row."
        );
    }

    #[test]
    fn the_schema_holds_no_room_for_content() {
        let f = Fixture::new("columns");
        let mut stmt = f
            .queue
            .conn
            .prepare("SELECT name FROM pragma_table_info('recordings')")
            .unwrap();
        let columns: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .map(|c| c.unwrap())
            .collect();

        assert_eq!(
            columns, ALLOWED_COLUMNS,
            "the queue's columns changed. Every column has to be argued for in \
             docs/spec-s3-retention.md before it exists -- transcript text, OCR \
             text, secret values and last-used timestamps are refused there by name."
        );
    }

    // --- test 7 -------------------------------------------------------------

    #[test]
    fn using_a_recording_again_does_not_buy_it_another_week() {
        let f = Fixture::new("noslide");
        let day = 24 * 60 * 60;
        f.ingest("a", Origin::Referenced, 0, "hello");

        // Re-ingested six days later: same video, same handle, new arrival time.
        let mut again = Recording::new("a", "C:/elsewhere/a.mp4", Origin::Referenced, 6 * day);
        again.status = Status::Ready;
        f.queue.put(&again).unwrap();

        let stored = f.queue.get("a").unwrap().unwrap();
        assert_eq!(stored.created_at, 0, "the clock slid");
        assert_eq!(
            stored.status,
            Status::Ready,
            "the rest of the row did not update"
        );
        assert_eq!(f.queue.expire(7 * day, &f.retention()).unwrap().len(), 1);
    }

    // --- test 8 -------------------------------------------------------------

    #[test]
    fn nothing_we_created_is_deleted_before_the_user_has_been_asked() {
        let f = Fixture::new("notasked");
        let r = f.ingest("ours", Origin::Copied, 0, "hello");

        let unasked = Retention::new(&f.recordings_dir); // choice_made: false
        assert!(unasked.delete_copied_sources, "the default is on");

        let day = 24 * 60 * 60;
        let purged = f.queue.expire(30 * day, &unasked).unwrap();

        assert_eq!(purged.len(), 1, "the row and its frames still expire");
        assert_eq!(purged[0].source, SourceOutcome::Left(LeaveReason::NotAsked));
        assert!(
            r.source_path.exists(),
            "a recording was deleted before anyone agreed to it"
        );
    }

    // --- the rest ----------------------------------------------------------

    #[test]
    fn a_failure_cannot_be_stored_without_saying_what_failed() {
        let f = Fixture::new("errorcheck");
        let mut r = Recording::new("a", "C:/x.mp4", Origin::Referenced, 0);
        r.status = Status::Error;
        r.error = None;
        assert!(f.queue.put(&r).is_err(), "a silent failure was accepted");

        r.error = Some("ffmpeg couldn't read that file. It may still be recording.".into());
        assert!(f.queue.put(&r).is_ok());
    }

    #[test]
    fn purging_something_that_is_not_here_is_not_an_error() {
        let f = Fixture::new("missing");
        assert!(f.queue.purge("nope", &f.retention()).unwrap().is_none());
    }

    /// The S3 definition of done: Cursor and Claude Code open together must not
    /// race on SQLite. Separate connections, because in the real system they
    /// are separate processes.
    #[test]
    fn two_writers_and_a_reader_do_not_collide() {
        let f = Fixture::new("race");
        let db = f.root.join("queue.db");
        let cache = f.root.join("cache");

        let writers: Vec<_> = (0..2)
            .map(|w| {
                let db = db.clone();
                let cache = cache.clone();
                std::thread::spawn(move || {
                    let q = Queue::open_at(&db, &cache).unwrap();
                    for i in 0..60 {
                        let r = Recording::new(
                            &format!("w{w}-{i}"),
                            format!("C:/videos/w{w}-{i}.mp4"),
                            Origin::Referenced,
                            i as i64,
                        );
                        q.put(&r).expect("concurrent write");
                    }
                })
            })
            .collect();

        let reader = {
            let db = db.clone();
            let cache = cache.clone();
            std::thread::spawn(move || {
                let q = Queue::open_at(&db, &cache).unwrap();
                for _ in 0..60 {
                    q.list(50).expect("concurrent read");
                }
            })
        };

        for w in writers {
            w.join().unwrap();
        }
        reader.join().unwrap();
        assert_eq!(f.queue.list(1000).unwrap().len(), 120);
    }

    #[test]
    fn the_list_is_newest_first() {
        let f = Fixture::new("order");
        for (handle, at) in [("a", 10), ("b", 30), ("c", 20)] {
            f.queue
                .put(&Recording::new(
                    handle,
                    format!("C:/{handle}.mp4"),
                    Origin::Referenced,
                    at,
                ))
                .unwrap();
        }
        let names: Vec<_> = f
            .queue
            .list(10)
            .unwrap()
            .into_iter()
            .map(|r| r.handle)
            .collect();
        assert_eq!(names, ["b", "c", "a"]);
    }
}
