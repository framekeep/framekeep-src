//! The retention policy, as code. Written from `docs/spec-s3-retention.md`,
//! which was settled before this file existed -- that order was the point.
//!
//! Everything here decides; nothing here deletes. [`Queue::purge`] does the
//! deleting, and it asks these functions first. Splitting the two means the
//! policy can be checked exhaustively without a filesystem full of victims,
//! and it means a decision to keep something can carry a reason -- the same
//! shape S1 uses for dropped frames, and for the same reason: a file that
//! vanishes without a recorded why is the worst kind of failure.
//!
//! The five laws, in the order they take precedence:
//!
//! - **L1** the queue holds no extracted content. Enforced by the schema, and
//!   by the column allowlist test in `queue.rs`.
//! - **L2** never delete a file Framekeep did not create. [`Origin`].
//! - **L3** the clock starts at ingest and does not slide. [`Retention::expired`].
//! - **L4** one deletion path. `Queue::purge`.
//! - **L5** deletable by default, but never silent. [`LeaveReason::NotAsked`].

use std::path::{Path, PathBuf};
use std::time::Duration;

/// Seven days, matching `Auto-delete completed recordings` in the settings copy.
pub const DEFAULT_KEEP_DAYS: u64 = 7;

/// Where a recording's bytes came from, and so who owns them.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Origin {
    /// The user pointed at a file that already existed. Theirs. Untouchable.
    Referenced,
    /// Framekeep wrote the bytes itself, from pasted video data.
    Copied,
}

impl Origin {
    pub fn as_str(self) -> &'static str {
        match self {
            Origin::Referenced => "referenced",
            Origin::Copied => "copied",
        }
    }

    /// No default. A row whose origin cannot be read is not quietly treated as
    /// `Copied`, because `Copied` is the one that can be deleted.
    pub fn parse(s: &str) -> Option<Origin> {
        match s {
            "referenced" => Some(Origin::Referenced),
            "copied" => Some(Origin::Copied),
            _ => None,
        }
    }
}

/// The user's settings, plus where their recordings live.
#[derive(Debug, Clone)]
pub struct Retention {
    pub keep_for: Duration,
    /// `Auto-delete completed recordings` in Settings.
    pub delete_copied_sources: bool,
    /// Whether the one-time question in L5b has been answered. Until it has,
    /// nothing Framekeep created is deleted, whatever the toggle says.
    pub choice_made: bool,
    /// `~/Framekeep/Recordings`. A file that has been moved out of here has been
    /// claimed by its owner.
    pub recordings_dir: PathBuf,
}

/// `~/Framekeep/Recordings` -- the visible folder, because a recording Framekeep
/// wrote may be the only copy, and nobody should have to know about a dotted
/// folder to find their own video.
pub fn default_recordings_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(|home| PathBuf::from(home).join("Framekeep").join("Recordings"))
}

impl Retention {
    /// The shipping defaults: seven days, deletion on -- and not yet agreed to,
    /// which is what makes "on" safe to be the default.
    pub fn new(recordings_dir: impl Into<PathBuf>) -> Retention {
        Retention {
            keep_for: Duration::from_secs(DEFAULT_KEEP_DAYS * 24 * 60 * 60),
            delete_copied_sources: true,
            choice_made: false,
            recordings_dir: recordings_dir.into(),
        }
    }

    /// L3. Measured from when the recording entered the queue, and from nothing
    /// else. There is deliberately no `last_used_at` to slide from: a window
    /// that slides means the recordings you open most live longest, which is an
    /// archive rather than a queue -- and it would make "nothing here is older
    /// than seven days" untrue while sounding truer.
    pub fn expired(&self, created_at: i64, now: i64) -> bool {
        now.saturating_sub(created_at) >= self.keep_for.as_secs() as i64
    }

    /// L2 and L5, in precedence order. The first rule that applies wins, and
    /// the strongest one is checked first.
    pub fn source_verdict(&self, origin: Origin, source_path: &Path) -> SourceVerdict {
        if origin != Origin::Copied {
            return SourceVerdict::Leave(LeaveReason::NotOurs);
        }
        if !self.choice_made {
            return SourceVerdict::Leave(LeaveReason::NotAsked);
        }
        if !self.delete_copied_sources {
            return SourceVerdict::Leave(LeaveReason::TurnedOff);
        }
        if !source_path.exists() {
            return SourceVerdict::Leave(LeaveReason::AlreadyGone);
        }
        if !inside(&self.recordings_dir, source_path) {
            return SourceVerdict::Leave(LeaveReason::MovedOut);
        }
        SourceVerdict::Delete
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceVerdict {
    Delete,
    Leave(LeaveReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaveReason {
    /// The user pointed at their own file. This one is not a setting.
    NotOurs,
    /// The one-time question has not been answered yet.
    NotAsked,
    /// `Auto-delete completed recordings` is off.
    TurnedOff,
    /// It is no longer in Framekeep's folder, so somebody took it.
    MovedOut,
    /// Nothing there to delete.
    AlreadyGone,
}

impl LeaveReason {
    pub fn as_str(self) -> &'static str {
        match self {
            LeaveReason::NotOurs => "the file belongs to you, Framekeep only read it",
            LeaveReason::NotAsked => "you have not been asked about auto-delete yet",
            LeaveReason::TurnedOff => "auto-delete is off",
            LeaveReason::MovedOut => "the file was moved out of Framekeep's folder",
            LeaveReason::AlreadyGone => "the file was already gone",
        }
    }
}

/// Is `path` inside `dir`?
///
/// Both sides are canonicalised first. On Windows a plain string prefix check
/// would be wrong in three ways -- case, short names, and symlinks -- and each
/// of those wrong answers deletes somebody's file.
fn inside(dir: &Path, path: &Path) -> bool {
    match (dir.canonicalize(), path.canonicalize()) {
        (Ok(dir), Ok(path)) => path.starts_with(dir),
        // A folder that does not exist contains nothing.
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("framekeep-retention-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn agreed(dir: &Path) -> Retention {
        Retention {
            choice_made: true,
            ..Retention::new(dir)
        }
    }

    /// The heaviest rule in the file: a file the user pointed at is never
    /// touched, whatever the settings say.
    #[test]
    fn a_referenced_file_is_never_deleted_under_any_setting() {
        let dir = temp("referenced");
        let file = dir.join("theirs.mp4");
        std::fs::write(&file, b"x").unwrap();

        for choice_made in [true, false] {
            for delete_copied_sources in [true, false] {
                let r = Retention {
                    choice_made,
                    delete_copied_sources,
                    ..Retention::new(&dir)
                };
                assert_eq!(
                    r.source_verdict(Origin::Referenced, &file),
                    SourceVerdict::Leave(LeaveReason::NotOurs),
                    "choice_made={choice_made} delete={delete_copied_sources}"
                );
            }
        }
    }

    /// L5b. The default is on, and that is only defensible because nothing is
    /// deleted before the user has been asked once.
    #[test]
    fn nothing_is_deleted_before_the_question_has_been_asked() {
        let dir = temp("notasked");
        let file = dir.join("ours.mp4");
        std::fs::write(&file, b"x").unwrap();

        let mut r = Retention::new(&dir);
        assert!(r.delete_copied_sources, "the default is on");
        assert!(!r.choice_made, "and the default is un-agreed");
        assert_eq!(
            r.source_verdict(Origin::Copied, &file),
            SourceVerdict::Leave(LeaveReason::NotAsked)
        );

        r.choice_made = true;
        assert_eq!(
            r.source_verdict(Origin::Copied, &file),
            SourceVerdict::Delete
        );
    }

    #[test]
    fn a_file_moved_out_of_our_folder_has_been_claimed() {
        let ours = temp("ours");
        let elsewhere = temp("elsewhere");
        let moved = elsewhere.join("recording.mp4");
        std::fs::write(&moved, b"x").unwrap();

        assert_eq!(
            agreed(&ours).source_verdict(Origin::Copied, &moved),
            SourceVerdict::Leave(LeaveReason::MovedOut)
        );
    }

    #[test]
    fn the_clock_starts_at_ingest_and_does_not_slide() {
        let r = Retention::new(temp("clock"));
        let day = 24 * 60 * 60;
        assert!(!r.expired(0, 6 * day));
        assert!(r.expired(0, 7 * day));
        // Nothing in this type can be told about a later use, which is the
        // point: there is no argument to slide.
        assert!(r.expired(0, 30 * day));
    }

    #[test]
    fn an_unreadable_origin_is_not_guessed_at() {
        assert_eq!(Origin::parse("copied"), Some(Origin::Copied));
        assert_eq!(Origin::parse("referenced"), Some(Origin::Referenced));
        assert_eq!(Origin::parse("COPIED"), None);
        assert_eq!(Origin::parse(""), None);
    }
}
