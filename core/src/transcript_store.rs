//! Where a transcript lives between the moment it is asked for and the moment
//! it is ready.
//!
//! # Why this exists
//!
//! Frames and words finish 85 times apart. Measured on a 126-second recording:
//! frames in 1.3 s, speech in 110.4 s. Running them concurrently saves about
//! 1% -- the real lever is not making the caller wait for the slow half at all
//! (`docs/experiments/whisper-speed.md`).
//!
//! That only works if the slow half has somewhere to land. Until now it had
//! nowhere: `transcribe` printed to stdout and forgot. So every `map` of the
//! same recording paid the full two minutes again, and an MCP adapter had no
//! way to answer "is it ready yet" without redoing the work. The queue database
//! that would otherwise hold this does not arrive until S3, and the MCP server
//! has to work without the tray anyway.
//!
//! # Shape on disk, in `~/.framekeep/cache/<handle>/`
//!
//! ```text
//! transcript.json          the result, plus what produced it
//! transcript.running       a lease: someone is working on it right now
//! transcript.failed.json   why the last attempt did not finish
//! ```
//!
//! One file per state rather than one file with a state field, because the
//! states are written by different processes at different times and a single
//! rewritten file is a race waiting to happen.
//!
//! Writes go to a `.partial` neighbour and are renamed into place, the same
//! discipline model downloads use: nothing appears at the real path until it is
//! known to be whole. A half-written transcript would not fail loudly -- it
//! would quietly hand the model a truncated account of what was said.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::transcribe::{Segment, Transcript};

const RESULT: &str = "transcript.json";
const LEASE: &str = "transcript.running";
const FAILURE: &str = "transcript.failed.json";

/// A transcript as it sits on disk: the words, plus enough about their origin
/// to decide whether they are still the ones you want.
#[derive(Debug, Serialize, Deserialize)]
pub struct StoredTranscript {
    pub has_audio: bool,
    /// Which model produced this. A transcript made by `tiny.en` and one made
    /// by the 547 MiB default are both valid and not interchangeable, and the
    /// caller is the only one who knows which it needs.
    pub model: Option<String>,
    /// Unix seconds. Stored rather than read from the file's mtime because a
    /// copy or a sync tool rewrites mtime without rewriting the transcript.
    pub created_unix: u64,
    pub segments: Vec<StoredSegment>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StoredSegment {
    pub start_seconds: f64,
    pub end_seconds: f64,
    pub text: String,
}

impl StoredTranscript {
    fn from(t: &Transcript) -> Self {
        StoredTranscript {
            has_audio: t.has_audio,
            model: t.model.clone(),
            created_unix: now_unix(),
            segments: t
                .segments
                .iter()
                .map(|s| StoredSegment {
                    start_seconds: s.start_seconds,
                    end_seconds: s.end_seconds,
                    text: s.text.clone(),
                })
                .collect(),
        }
    }

    pub fn into_transcript(self) -> Transcript {
        Transcript {
            has_audio: self.has_audio,
            model: self.model,
            segments: self
                .segments
                .into_iter()
                .map(|s| Segment {
                    start_seconds: s.start_seconds,
                    end_seconds: s.end_seconds,
                    text: s.text,
                })
                .collect(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct Lease {
    pid: u32,
    started_unix: u64,
    /// When to consider this lease abandoned. Derived from how long the work
    /// should take, not a fixed number: a one-minute clip and a one-hour
    /// recording do not deserve the same patience.
    expires_unix: u64,
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Status {
    /// Nobody has asked for this transcript yet.
    Absent,
    /// A process is working on it. `stale` means the lease outlived its
    /// deadline, which usually means that process was killed.
    Running {
        since_unix: u64,
        stale: bool,
    },
    Ready(StoredTranscript),
    Failed {
        error: String,
        at_unix: u64,
    },
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredFailure {
    error: String,
    at_unix: u64,
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Reads the current state without starting or changing anything.
///
/// Read-only on purpose: an MCP adapter polls this, and a status check that
/// quietly kicks off two minutes of work would be a trap.
pub fn status(dir: &Path) -> Status {
    if let Ok(bytes) = std::fs::read(dir.join(RESULT)) {
        if let Ok(t) = serde_json::from_slice::<StoredTranscript>(&bytes) {
            return Status::Ready(t);
        }
        // Unreadable result: say so rather than reporting Absent, which would
        // send the caller off to redo work whose output is sitting right there
        // in a form nobody can parse. That difference matters when debugging.
        return Status::Failed {
            error: format!(
                "{} exists but could not be read as a transcript",
                dir.join(RESULT).display()
            ),
            at_unix: now_unix(),
        };
    }

    if let Ok(bytes) = std::fs::read(dir.join(LEASE)) {
        if let Ok(l) = serde_json::from_slice::<Lease>(&bytes) {
            return Status::Running {
                since_unix: l.started_unix,
                stale: now_unix() > l.expires_unix,
            };
        }
    }

    if let Ok(bytes) = std::fs::read(dir.join(FAILURE)) {
        if let Ok(f) = serde_json::from_slice::<StoredFailure>(&bytes) {
            return Status::Failed {
                error: f.error,
                at_unix: f.at_unix,
            };
        }
    }

    Status::Absent
}

/// Held while a process transcribes. Dropping it releases the claim.
pub struct Claim {
    dir: PathBuf,
    released: bool,
}

impl Claim {
    /// Marks the work finished and stores the result.
    pub fn finish(mut self, transcript: &Transcript) -> std::io::Result<()> {
        let stored = StoredTranscript::from(transcript);
        write_atomic(
            &self.dir.join(RESULT),
            &serde_json::to_vec_pretty(&stored).map_err(std::io::Error::other)?,
        )?;
        let _ = std::fs::remove_file(self.dir.join(FAILURE));
        self.release();
        Ok(())
    }

    /// Records why the attempt did not finish, so the next caller learns
    /// something instead of starting the same doomed work again.
    pub fn fail(mut self, error: &str) -> std::io::Result<()> {
        let f = StoredFailure {
            error: error.to_string(),
            at_unix: now_unix(),
        };
        write_atomic(
            &self.dir.join(FAILURE),
            &serde_json::to_vec_pretty(&f).map_err(std::io::Error::other)?,
        )?;
        self.release();
        Ok(())
    }

    fn release(&mut self) {
        if !self.released {
            let _ = std::fs::remove_file(self.dir.join(LEASE));
            self.released = true;
        }
    }
}

impl Drop for Claim {
    /// Covers the ordinary exits and most panics. A hard kill still leaves the
    /// lease behind, which is what the expiry in [`claim`] is for.
    fn drop(&mut self) {
        self.release();
    }
}

#[derive(Debug)]
pub enum ClaimError {
    /// Someone else is already transcribing this recording. Not an error the
    /// user caused -- two MCP clients open on the same video is ordinary.
    AlreadyRunning {
        since_unix: u64,
    },
    Io(std::io::Error),
}

impl fmt::Display for ClaimError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ClaimError::AlreadyRunning { since_unix } => write!(
                f,
                "Another Framekeep process has been transcribing this recording for {}s.\n\
                 Wait for it to finish -- the result is shared, so this is not wasted time.\n\
                 Check progress with: framekeep-core transcript <path>",
                now_unix().saturating_sub(*since_unix)
            ),
            ClaimError::Io(e) => write!(f, "Couldn't claim the transcript slot: {e}"),
        }
    }
}

/// Takes the right to transcribe this recording, or reports who has it.
///
/// `expected_seconds` is how long the work should take -- pass the video's
/// duration. The lease expires at three times that, floored at ten minutes, so
/// a process killed mid-run blocks retries for a while rather than forever, and
/// a long recording is not declared abandoned while it is still going.
pub fn claim(dir: &Path, expected_seconds: f64) -> Result<Claim, ClaimError> {
    std::fs::create_dir_all(dir).map_err(ClaimError::Io)?;
    let lease_path = dir.join(LEASE);

    // Atomic: whoever creates the file wins, and the loser is told so rather
    // than both processes running whisper over the same audio.
    match std::fs::File::create_new(&lease_path) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let existing = std::fs::read(&lease_path)
                .ok()
                .and_then(|b| serde_json::from_slice::<Lease>(&b).ok());
            match existing {
                // An expired lease is taken over rather than respected: the
                // process that held it is almost certainly gone, and refusing
                // forever would strand the recording.
                Some(l) if now_unix() > l.expires_unix => {}
                Some(l) => {
                    return Err(ClaimError::AlreadyRunning {
                        since_unix: l.started_unix,
                    })
                }
                // Unparseable lease: treat as debris and take over. It cannot
                // tell us who holds it, so honouring it protects nobody.
                None => {}
            }
        }
        Err(e) => return Err(ClaimError::Io(e)),
    }

    let now = now_unix();
    let patience = ((expected_seconds * 3.0).max(600.0)) as u64;
    let lease = Lease {
        pid: std::process::id(),
        started_unix: now,
        expires_unix: now + patience,
    };
    std::fs::write(
        &lease_path,
        serde_json::to_vec_pretty(&lease).map_err(|e| ClaimError::Io(std::io::Error::other(e)))?,
    )
    .map_err(ClaimError::Io)?;

    // A process killed mid-transcription cannot run its own destructors, so the
    // audio copy it left is swept here -- by the one process that has just won
    // the exclusive right to that folder, and is about to rewrite the file
    // anyway.
    crate::transcribe::sweep_stranded_audio(dir);

    Ok(Claim {
        dir: dir.to_path_buf(),
        released: false,
    })
}

/// Writes through a `.partial` neighbour so the real path never holds a
/// half-written file.
fn write_atomic(dest: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let partial = dest.with_extension("partial");
    std::fs::write(&partial, bytes)?;
    match std::fs::rename(&partial, dest) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&partial);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "framekeep-store-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn sample() -> Transcript {
        Transcript {
            has_audio: true,
            model: Some("tiny.en".into()),
            segments: vec![Segment {
                start_seconds: 0.0,
                end_seconds: 1.5,
                text: "hello".into(),
            }],
        }
    }

    #[test]
    fn nothing_stored_reads_as_absent() {
        let dir = temp("absent");
        assert!(matches!(status(&dir), Status::Absent));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_finished_claim_reads_back_as_ready() {
        let dir = temp("ready");
        claim(&dir, 10.0).unwrap().finish(&sample()).unwrap();

        match status(&dir) {
            Status::Ready(t) => {
                assert_eq!(t.segments.len(), 1);
                assert_eq!(t.model.as_deref(), Some("tiny.en"));
                assert!(t.has_audio);
            }
            other => panic!("expected Ready, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_live_claim_blocks_a_second_one() {
        let dir = temp("contended");
        let held = claim(&dir, 10.0).unwrap();

        // Two MCP clients on the same recording is ordinary, not an error, and
        // the second must not start whisper over the same audio.
        assert!(matches!(
            claim(&dir, 10.0),
            Err(ClaimError::AlreadyRunning { .. })
        ));
        assert!(matches!(status(&dir), Status::Running { stale: false, .. }));

        drop(held);
        // Released: the slot is free again without anyone cleaning up by hand.
        assert!(claim(&dir, 10.0).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_expired_lease_is_taken_over_rather_than_honoured_forever() {
        let dir = temp("stale");
        let stale = Lease {
            pid: 1,
            started_unix: 0,
            expires_unix: 1,
        };
        std::fs::write(dir.join(LEASE), serde_json::to_vec_pretty(&stale).unwrap()).unwrap();

        assert!(matches!(status(&dir), Status::Running { stale: true, .. }));
        assert!(
            claim(&dir, 10.0).is_ok(),
            "a killed process must not strand a recording forever"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failure_is_recorded_instead_of_looking_like_nothing_happened() {
        let dir = temp("failed");
        claim(&dir, 10.0)
            .unwrap()
            .fail("whisper ran out of memory")
            .unwrap();

        match status(&dir) {
            Status::Failed { error, .. } => assert!(error.contains("out of memory")),
            other => panic!("expected Failed, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_later_success_clears_an_earlier_failure() {
        let dir = temp("recovered");
        claim(&dir, 10.0).unwrap().fail("transient").unwrap();
        claim(&dir, 10.0).unwrap().finish(&sample()).unwrap();

        // Ready takes precedence, and the stale failure is gone rather than
        // sitting there waiting to confuse whoever reads the folder next.
        assert!(matches!(status(&dir), Status::Ready(_)));
        assert!(!dir.join(FAILURE).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unreadable_result_is_not_reported_as_absent() {
        let dir = temp("corrupt");
        std::fs::write(dir.join(RESULT), b"{ this is not json").unwrap();

        // Absent would send the caller off to redo two minutes of work whose
        // output is right there, just unreadable. Saying so is more useful.
        match status(&dir) {
            Status::Failed { error, .. } => assert!(error.contains("could not be read")),
            other => panic!("expected Failed, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nothing_appears_at_the_real_path_until_it_is_whole() {
        let dir = temp("atomic");
        let dest = dir.join(RESULT);
        write_atomic(&dest, b"complete").unwrap();

        assert_eq!(std::fs::read(&dest).unwrap(), b"complete");
        assert!(
            !dir.join("transcript.partial").exists(),
            "the partial neighbour must not survive a successful write"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
