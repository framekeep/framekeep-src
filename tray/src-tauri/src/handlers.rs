//! The methods, once a call has passed the boundary in `method.rs`. This is S3.5.
//!
//! Two things are enforced here and nowhere else:
//!
//! - **The review gate.** A recording with findings the user has not approved
//!   is not readable, by anyone, through any method. It is built now rather
//!   than with the rest of redaction in S5 for the same reason the transcript
//!   fence shipped in v1: a gate added afterwards is a gate that was open for a
//!   release.
//! - **Nothing extracted comes back through the queue.** These replies carry
//!   counts, stages and file names. Frames and transcripts travel by path, and
//!   only through `video.map` / `video.frames`, which is where the fences that
//!   handle untrusted content already live.
//!
//! One `QueueHandlers` per connection, so each has its own SQLite handle. That
//! is also how it works between processes, which is the case that matters.

use crate::method::Method;
use crate::protocol::ErrorCode;
use crate::queue::{Queue, QueueError, Recording, Status};
use crate::retention::{Origin, Retention, SourceVerdict};
use crate::session::Handlers;
use serde_json::{json, Value};

pub struct QueueHandlers {
    queue: Queue,
    retention: Retention,
}

impl QueueHandlers {
    pub fn new(queue: Queue, retention: Retention) -> QueueHandlers {
        QueueHandlers { queue, retention }
    }
}

type Answer = Result<Value, (ErrorCode, String)>;

/// Used when the queue itself could not be opened for a connection. It says so
/// on every call rather than falling back to something that looks like it
/// worked -- a server that answers "not ready" while its database is broken
/// teaches a client to wait for something that will never arrive.
pub struct Unavailable(pub String);

impl Handlers for Unavailable {
    fn call(&mut self, _method: Method, _params: &Value) -> Answer {
        Err((ErrorCode::CoreFailed, self.0.clone()))
    }
}

impl Handlers for QueueHandlers {
    fn capabilities(&self) -> Vec<&'static str> {
        // What this build can actually do. A client reads this instead of
        // guessing from the version, so it may only claim what is true today.
        // `frames` is the promise that matters: this app can serve the frames
        // of a queued recording, redacted where a person masked them.
        vec!["queue", "ingest", "frames", "redaction"]
    }

    fn call(&mut self, method: Method, params: &Value) -> Answer {
        match method {
            Method::QueueList => self.list(params),
            Method::QueueGet => self.get(params),
            Method::VideoStatus => self.status(params),
            Method::VideoMap => self.map(params),
            Method::VideoFrames => self.frames(params),

            // The model never reaches this -- `method.rs` refuses it before a
            // handler is consulted, and that refusal is the point of the
            // slice. What arrives here came from a person at the window.
            Method::VideoIngest => self.ingest(params),

            Method::RedactionApply => Err((
                ErrorCode::NotReady,
                "Redaction review arrives with a later build. Nothing is scanned or hidden yet."
                    .to_string(),
            )),

            // Genuinely unreachable: the session answers `hello` itself and
            // never dispatches it. Answering rather than panicking, because a
            // panic here would take down a live connection.
            Method::Hello => Err((
                ErrorCode::CoreFailed,
                "hello reached the queue, which should be impossible. Please report this."
                    .to_string(),
            )),
        }
    }
}

impl QueueHandlers {
    /// Public because the window's snapshot command is this exact call: the
    /// IPC surface and the GUI read the queue through one door, so they cannot
    /// disagree about what a row looks like.
    pub fn list(&self, params: &Value) -> Answer {
        let limit = params
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(20)
            .clamp(1, 200) as usize;

        let items = self.queue.list(limit).map_err(db)?;
        let total = items.len();
        Ok(json!({
            "items": items.iter().map(|r| self.summary(r)).collect::<Vec<_>>(),
            "showing": total,
        }))
    }

    fn get(&self, params: &Value) -> Answer {
        let recording = self.require(params)?;
        Ok(self.summary(&recording))
    }

    fn status(&self, params: &Value) -> Answer {
        let r = self.require(params)?;
        Ok(json!({
            "handle": r.handle,
            "stage": r.status.as_str(),
            "frames_found": r.frame_count,
            "sensitive_items": r.finding_count,
            "error": r.error,
        }))
    }

    /// Put a recording into the queue. S4, and the write half of the boundary.
    ///
    /// `origin` decides whether Framekeep may ever delete the file again, so it
    /// is taken from the caller rather than guessed: only the paste path knows
    /// whether it wrote those bytes itself or was handed a path. Anything it
    /// does not say is `referenced`, the safe answer -- the retention rules
    /// treat that as untouchable.
    pub fn ingest(&mut self, params: &Value) -> Answer {
        let path = params.get("path").and_then(Value::as_str).ok_or((
            ErrorCode::BadRequest,
            "Ingest needs a `path` to the recording.".to_string(),
        ))?;
        let path = std::path::PathBuf::from(path);

        let origin = match params.get("origin").and_then(Value::as_str) {
            Some("copied") => Origin::Copied,
            _ => Origin::Referenced,
        };

        // The handle is core's, computed the same way core computes it, so the
        // queue row and the cache folder are about the same thing. Re-adding a
        // recording finds the existing row instead of making a second one.
        let handle = crate::handle::for_path(&path).map_err(|e| {
            (
                ErrorCode::NotFound,
                format!(
                    "There's no readable file at {} ({e}). Check the path, then try again.",
                    path.display()
                ),
            )
        })?;

        // An entry that is already here keeps its arrival time: re-pasting the
        // same recording must not buy it another seven days (L3).
        if let Some(existing) = self.queue.get(&handle).map_err(db)? {
            return Ok(json!({
                "handle": existing.handle,
                "already_queued": true,
                "recording": self.summary(&existing),
            }));
        }

        let recording = Recording::new(&handle, &path, origin, crate::queue::now_unix());
        self.queue.put(&recording).map_err(db)?;

        Ok(json!({
            "handle": handle,
            "already_queued": false,
            "recording": self.summary(&recording),
        }))
    }

    /// Which frames of a queued recording a caller may read, and where they are.
    ///
    /// This is the bridge S3 left open, and what it exists for is one sentence:
    /// **a recording that went through the app is served from its redacted
    /// copies.** The adapter can already reach `framekeep-core` itself -- that is
    /// the standalone path, and it is honest about having skipped review. What
    /// it cannot do on its own is know which frames a person masked. That
    /// knowledge lives here, so it is answered here.
    ///
    /// Deliberately NOT a second implementation of the map: no ffmpeg, no
    /// selection, no re-transcribing. The pipeline already produced all of
    /// that; this reads what is on disk and substitutes the masked files.
    /// Re-deriving any of it would be the duplicate logic `AGENTS.md` warns
    /// about, and the two copies would drift on the day one of them was fixed.
    ///
    /// The transcript rides along for a reason that only showed up when the
    /// adapter was read end to end: without it, the adapter had to call
    /// `framekeep-core` for the words even when the app already had them. That
    /// made core a hard requirement of the ordinary path -- and an installed
    /// app keeps its core inside the package directory, where a separate Node
    /// process has no business reaching. So an app that answers this call
    /// answers all of it, and core stays what it was meant to be: the
    /// standalone path, for people who never installed the app.
    fn map(&self, params: &Value) -> Answer {
        let recording = self.require(params)?;
        // A cache whose files are all gone is reported, not served as an
        // empty recording -- see `Queue::frames_checked`.
        let frames = self
            .queue
            .frames_checked(&recording.handle)
            .map_err(|why| (ErrorCode::CoreFailed, why))?;
        Ok(json!({
            "handle": recording.handle,
            "source_path": recording.source_path.to_string_lossy(),
            "video": {
                "width": recording.width,
                "height": recording.height,
                "duration_ms": recording.duration_ms,
            },
            "transcript": self.transcript(&recording),
            "frames": frames.iter().map(frame_json).collect::<Vec<_>>(),
            "frame_count": frames.len(),
            // What review did, in numbers a caller can act on. `found` with
            // `hidden: 0` is a person who looked and chose to hide nothing --
            // a different fact from nothing having been found, and the reply
            // has to keep them apart.
            "review": {
                "reviewed_at": recording.reviewed_at,
                "found": recording.finding_count,
                "hidden": frames.iter().filter(|f| f.redacted).count(),
            },
        }))
    }

    /// What the pipeline's transcript stage has to show for itself, in the
    /// shape the adapter already understands (`core`'s `TranscriptStatus`).
    ///
    /// Read from `transcript.json` rather than kept in the queue row, because
    /// core wrote that file and core owns its shape. A row field would be a
    /// copy, and a copy of a file is a thing that can disagree with it.
    ///
    /// Three answers, and the middle one is why this is not just "read the
    /// file or return null": a recording still being transcribed has no file
    /// yet and is not absent either. Telling those apart is what stops a
    /// caller reporting "no speech in this recording" about a recording whose
    /// words are four minutes away.
    fn transcript(&self, recording: &Recording) -> Value {
        if recording.status == Status::Transcribing {
            return json!({
                "status": "running",
                "since_unix": recording.created_at,
                "stale": false,
            });
        }
        let path = self
            .queue
            .cache_dir(&recording.handle)
            .join("transcript.json");
        let Ok(text) = std::fs::read_to_string(&path) else {
            return json!({ "status": "absent" });
        };
        let Ok(mut parsed) = serde_json::from_str::<Value>(&text) else {
            return json!({ "status": "absent" });
        };
        if let Some(obj) = parsed.as_object_mut() {
            obj.insert("status".into(), json!("ready"));
        }
        parsed
    }

    /// The same frames, narrowed to a time range.
    ///
    /// Bounds are seconds and both optional; an absent bound means "from the
    /// start" / "to the end". A range that selects nothing is an empty list and
    /// a sentence, not an error: asking about a quiet stretch of a recording is
    /// a reasonable question with a boring answer.
    fn frames(&self, params: &Value) -> Answer {
        let recording = self.require(params)?;
        let from = params.get("from_seconds").and_then(Value::as_f64);
        let to = params.get("to_seconds").and_then(Value::as_f64);
        if let (Some(a), Some(b)) = (from, to) {
            if b < a {
                return Err((
                    ErrorCode::BadRequest,
                    format!("from_seconds ({a}) is after to_seconds ({b}); nothing can be in that range."),
                ));
            }
        }

        let all = self
            .queue
            .frames_checked(&recording.handle)
            .map_err(|why| (ErrorCode::CoreFailed, why))?;
        let picked: Vec<&crate::queue::Frame> = all
            .iter()
            .filter(|f| from.is_none_or(|a| f.pts_time >= a))
            .filter(|f| to.is_none_or(|b| f.pts_time <= b))
            .collect();

        Ok(json!({
            "handle": recording.handle,
            // Paths, never pixels. The house rule everywhere frames are
            // mentioned: images travel as files, and whoever wants bytes reads
            // them itself.
            "frames": picked.iter().map(|f| frame_json(f)).collect::<Vec<_>>(),
            "frames_in_range": picked.len(),
            "frames_total": all.len(),
        }))
    }

    /// Look a recording up, and refuse it if the user has not reviewed it.
    ///
    /// The order is the design: not found, then awaiting review, then anything
    /// else. Answering "not ready" for an unreviewed recording would invite a
    /// retry loop against the one state that only a person can clear.
    fn require(&self, params: &Value) -> Result<Recording, (ErrorCode, String)> {
        // `handle` or `path`. The adapter is handed a path by the model and has
        // no handle to offer; computing one here uses the same fingerprint core
        // and the paste path use, so all three agree about what "this
        // recording" means -- and a path that was never queued comes back
        // NOT_FOUND, which is exactly the signal the adapter needs in order to
        // fall back to reading the file itself and say review was skipped.
        let owned;
        let handle = match params.get("handle").and_then(Value::as_str) {
            Some(h) => h,
            None => {
                let path = params.get("path").and_then(Value::as_str).ok_or((
                    ErrorCode::BadRequest,
                    "That call needs a `handle` or a `path`. Get a handle from `queue.list`."
                        .to_string(),
                ))?;
                owned = crate::handle::for_path(std::path::Path::new(path)).map_err(|e| {
                    (
                        ErrorCode::NotFound,
                        format!("There's no readable file at {path} ({e})."),
                    )
                })?;
                &owned
            }
        };

        let recording = self.queue.get(handle).map_err(db)?.ok_or_else(|| {
            (
                ErrorCode::NotFound,
                format!(
                    "No recording called `{handle}` in Framekeep. \
                     It may have been removed -- `queue.list` shows what is there."
                ),
            )
        })?;

        if recording.status == Status::NeedsReview {
            let count = recording.finding_count.unwrap_or(0);
            let items = if count == 1 { "item" } else { "items" };
            return Err((
                ErrorCode::AwaitingReview,
                format!(
                    "This recording is waiting for your review in Framekeep -- {count} sensitive {items} \
                     were detected. Approve them and ask me again."
                ),
            ));
        }

        Ok(recording)
    }
}

impl QueueHandlers {
    /// What a recording looks like on the wire.
    ///
    /// File names go in a data field and never into a sentence the server
    /// writes in its own voice. Somebody else named the video that was sent to
    /// this user, so the name is their text, not ours -- the same reasoning the
    /// transcript fence rests on, one notch weaker.
    fn summary(&self, r: &Recording) -> Value {
        // L5c: the countdown belongs next to the data, not buried in settings.
        // Two separate facts, because they are separate promises -- the entry
        // always stops being remembered, while the file itself is only ever
        // touched when Framekeep wrote it and the user has agreed.
        let expires_at = r.created_at + self.retention.keep_for.as_secs() as i64;
        let source_will_be_deleted = matches!(
            self.retention.source_verdict(r.origin, &r.source_path),
            SourceVerdict::Delete
        );

        json!({
            "handle": r.handle,
            "name": r.display_name,
            "stage": r.status.as_str(),
            "created_at": r.created_at,
            "duration_ms": r.duration_ms,
            "width": r.width,
            "height": r.height,
            "frames_found": r.frame_count,
            "sensitive_items": r.finding_count,
            "awaiting_review": r.status == Status::NeedsReview,
            "source_is_ours": r.origin == Origin::Copied,
            "expires_at": expires_at,
            "source_will_be_deleted": source_will_be_deleted,
            "error": r.error,
            // A path, not pixels -- the house rule for frames everywhere.
            "thumbnail": self.queue.first_frame(&r.handle),
        })
    }
}

/// A frame on the wire. `redacted` is stated rather than implied, because a
/// caller has to be able to tell "a person masked this" from "nothing needed
/// masking" -- they look identical in the pixels and mean opposite things
/// about how much review this recording has had.
fn frame_json(f: &crate::queue::Frame) -> Value {
    json!({
        "pts_time": f.pts_time,
        "file": f.file.to_string_lossy(),
        "redacted": f.redacted,
    })
}

fn db(e: QueueError) -> (ErrorCode, String) {
    (ErrorCode::CoreFailed, e.to_string())
}

/// Expire what is past its keep-for and sweep the litter, then say what
/// happened. Called at startup and from the tray's own timer later.
///
/// It returns a sentence rather than logging one, because a deletion nobody can
/// see is the failure mode this whole policy exists to avoid.
pub fn run_retention(queue: &Queue, retention: &Retention, now: i64) -> Result<String, QueueError> {
    let purged = queue.expire(now, retention)?;
    let swept = queue.sweep_orphans()?;

    let sources_deleted = purged
        .iter()
        .filter(|p| matches!(p.source, crate::queue::SourceOutcome::Deleted))
        .count();

    Ok(format!(
        "Retention: {} expired, {} recording files removed, {} empty folders swept.",
        purged.len(),
        sources_deleted,
        swept.len()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::queue::Recording;
    use crate::session::Session;

    fn fixture(name: &str) -> (QueueHandlers, std::path::PathBuf) {
        let root =
            std::env::temp_dir().join(format!("framekeep-handlers-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let queue = Queue::open_at(root.join("queue.db"), root.join("cache")).unwrap();
        let retention = Retention::new(root.join("Recordings"));
        (QueueHandlers::new(queue, retention), root)
    }

    fn session_with(handlers: QueueHandlers) -> Session {
        let mut s = Session::new(Box::new(handlers));
        s.handle_line(
            br#"{"id":"0","method":"hello","params":{"client":"framekeep-mcp","protocol":1}}"#,
        );
        s
    }

    fn send(s: &mut Session, line: &str) -> Value {
        serde_json::from_str(&s.handle_line(line.as_bytes()).to_line()).unwrap()
    }

    #[test]
    fn the_queue_comes_back_newest_first_with_counts_not_content() {
        let (h, _root) = fixture("list");
        for (handle, at) in [("a", 10), ("b", 20)] {
            let mut r =
                Recording::new(handle, format!("C:/v/{handle}.mp4"), Origin::Referenced, at);
            r.status = Status::Ready;
            r.frame_count = Some(11);
            h.queue.put(&r).unwrap();
        }

        let mut s = session_with(h);
        let reply = send(&mut s, r#"{"id":"1","method":"queue.list","params":{}}"#);
        let items = reply["result"]["items"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["name"], "b.mp4");
        assert_eq!(items[0]["frames_found"], 11);
        // Nothing that could carry what was said or shown.
        for key in ["transcript", "text", "segments", "findings"] {
            assert!(items[0].get(key).is_none(), "the queue leaked `{key}`");
        }
    }

    /// L5c. The countdown has to be answerable without opening Settings, and it
    /// has to be honest about which of the two promises it is making.
    #[test]
    fn a_row_says_when_it_expires_and_whether_the_file_goes_with_it() {
        let (mut h, root) = fixture("countdown");
        let recordings = root.join("Recordings");
        std::fs::create_dir_all(&recordings).unwrap();

        let ours = recordings.join("ours.mp4");
        std::fs::write(&ours, b"x").unwrap();
        h.queue
            .put(&Recording::new("ours", &ours, Origin::Copied, 0))
            .unwrap();
        let theirs = root.join("theirs.mp4");
        std::fs::write(&theirs, b"x").unwrap();
        h.queue
            .put(&Recording::new("theirs", &theirs, Origin::Referenced, 0))
            .unwrap();

        // Nobody has agreed yet, so nothing is promised to be deleted.
        let before = h.summary(&h.queue.get("ours").unwrap().unwrap());
        assert_eq!(before["expires_at"], 7 * 24 * 60 * 60);
        assert_eq!(before["source_will_be_deleted"], false);

        h.retention.choice_made = true;
        assert_eq!(
            h.summary(&h.queue.get("ours").unwrap().unwrap())["source_will_be_deleted"],
            true
        );
        // A file the user pointed at never gets a countdown, whatever they agreed.
        assert_eq!(
            h.summary(&h.queue.get("theirs").unwrap().unwrap())["source_will_be_deleted"],
            false
        );
    }

    /// S5.7, wired now so it cannot be forgotten later.
    #[test]
    fn an_unreviewed_recording_is_not_readable_by_any_method() {
        let (h, _root) = fixture("gate");
        let mut r = Recording::new("secret", "C:/v/secret.mp4", Origin::Referenced, 0);
        r.status = Status::NeedsReview;
        r.finding_count = Some(2);
        h.queue.put(&r).unwrap();

        let mut s = session_with(h);
        for method in ["queue.get", "video.status", "video.map", "video.frames"] {
            let reply = send(
                &mut s,
                &format!(r#"{{"id":"1","method":"{method}","params":{{"handle":"secret"}}}}"#),
            );
            assert_eq!(
                reply["error"]["code"], "AWAITING_REVIEW",
                "{method} served an unreviewed recording: {reply}"
            );
            // And the message points at the person, not at a retry.
            let message = reply["error"]["message"].as_str().unwrap();
            assert!(message.contains("2 sensitive items"), "{message}");
            assert!(message.contains("Approve"), "{message}");
        }
    }

    #[test]
    fn the_review_gate_is_checked_before_the_not_ready_answer() {
        // Otherwise an unreviewed recording would read as "try again later" the
        // moment video.map starts working.
        let (h, _root) = fixture("order");
        let mut r = Recording::new("x", "C:/v/x.mp4", Origin::Referenced, 0);
        r.status = Status::NeedsReview;
        h.queue.put(&r).unwrap();
        let mut s = session_with(h);
        let reply = send(
            &mut s,
            r#"{"id":"1","method":"video.map","params":{"handle":"x"}}"#,
        );
        assert_eq!(reply["error"]["code"], "AWAITING_REVIEW");
    }

    /// The GUI is allowed to ingest. That it cannot yet is a different fact
    /// from being refused, and telling the two apart is the difference between
    /// "wait for the next version" and "stop asking".
    #[test]
    fn the_gui_can_ingest_and_the_model_still_cannot() {
        let (h, root) = fixture("tray_ingest");
        let video = root.join("demo.mp4");
        std::fs::write(&video, b"not really a video").unwrap();

        let mut tray = Session::new(Box::new(h));
        tray.handle_line(
            br#"{"id":"0","method":"hello","params":{"client":"framekeep-tray","protocol":1}}"#,
        );
        let reply = send(
            &mut tray,
            &format!(
                r#"{{"id":"1","method":"video.ingest","params":{{"path":{}}}}}"#,
                serde_json::to_string(&video.to_string_lossy()).unwrap()
            ),
        );
        assert!(reply["error"].is_null(), "{reply}");
        assert_eq!(reply["result"]["already_queued"], false);
        assert_eq!(reply["result"]["recording"]["name"], "demo.mp4");

        let (h2, _root2) = fixture("mcp_ingest");
        let mut mcp = session_with(h2);
        let reply = send(
            &mut mcp,
            r#"{"id":"1","method":"video.ingest","params":{"path":"C:/x.mp4"}}"#,
        );
        assert_eq!(reply["error"]["code"], "FORBIDDEN", "{reply}");
    }

    /// L3, at the door this time: pasting the same recording twice must not
    /// restart its seven days, and must not make a second row.
    #[test]
    fn pasting_the_same_recording_twice_finds_the_first_one() {
        let (mut h, root) = fixture("reingest");
        let video = root.join("again.mp4");
        std::fs::write(&video, b"x").unwrap();
        let params = json!({ "path": video.to_string_lossy() });

        let first = h.ingest(&params).unwrap();
        assert_eq!(first["already_queued"], false);
        let handle = first["handle"].as_str().unwrap().to_string();
        let created = h.queue.get(&handle).unwrap().unwrap().created_at;

        let second = h.ingest(&params).unwrap();
        assert_eq!(second["already_queued"], true);
        assert_eq!(second["handle"], handle, "a second row appeared");
        assert_eq!(
            h.queue.get(&handle).unwrap().unwrap().created_at,
            created,
            "the clock was restarted by re-pasting"
        );
        assert_eq!(h.queue.list(10).unwrap().len(), 1);
    }

    /// `origin` decides whether Framekeep may ever delete those bytes, so an
    /// unstated one has to mean the untouchable answer.
    #[test]
    fn a_recording_is_the_users_unless_the_caller_says_it_made_it() {
        let (mut h, root) = fixture("origin");
        let video = root.join("theirs.mp4");
        std::fs::write(&video, b"x").unwrap();

        let reply = h
            .ingest(&json!({ "path": video.to_string_lossy() }))
            .unwrap();
        let handle = reply["handle"].as_str().unwrap();
        assert_eq!(
            h.queue.get(handle).unwrap().unwrap().origin,
            Origin::Referenced,
            "an unstated origin must be the one that is never deleted"
        );
    }

    #[test]
    fn ingesting_something_that_is_not_there_says_which_path() {
        let (mut h, _root) = fixture("nofile");
        let (code, message) = h
            .ingest(&json!({ "path": r"C:\definitely\not\here.mp4" }))
            .unwrap_err();
        assert_eq!(code, ErrorCode::NotFound);
        assert!(message.contains("here.mp4"), "{message}");
        assert!(message.contains("try again"), "{message}");
    }

    #[test]
    fn a_handle_nobody_has_seen_says_where_to_look() {
        let (h, _root) = fixture("missing");
        let mut s = session_with(h);
        let reply = send(
            &mut s,
            r#"{"id":"1","method":"queue.get","params":{"handle":"nope"}}"#,
        );
        assert_eq!(reply["error"]["code"], "NOT_FOUND");
        assert!(reply["error"]["message"]
            .as_str()
            .unwrap()
            .contains("queue.list"));
    }

    #[test]
    fn a_call_with_no_handle_says_where_to_get_one() {
        let (h, _root) = fixture("nohandle");
        let mut s = session_with(h);
        let reply = send(&mut s, r#"{"id":"1","method":"video.status","params":{}}"#);
        assert_eq!(reply["error"]["code"], "BAD_REQUEST");
    }

    #[test]
    fn a_reviewed_recording_is_served_from_its_redacted_copies() {
        // The bridge's whole reason to exist: once a person has masked a
        // frame, the ONLY path this door will name is the masked one.
        let (h, root) = fixture("bridge");
        let cache = h.queue.cache_dir("r1");
        std::fs::create_dir_all(cache.join("redacted")).unwrap();
        let original = cache.join("frame-00001.webp");
        let plain = cache.join("frame-00002.webp");
        std::fs::write(&original, b"original pixels").unwrap();
        std::fs::write(&plain, b"nothing to hide here").unwrap();
        std::fs::write(cache.join("redacted").join("frame-00001.webp"), b"masked").unwrap();
        std::fs::write(
            cache.join("frames.json"),
            serde_json::json!({
                "frames": [
                    { "pts_time": 0.0, "file": original },
                    { "pts_time": 5.0, "file": plain },
                ]
            })
            .to_string(),
        )
        .unwrap();

        let mut row = Recording::new("r1", "C:/v/r1.mp4", Origin::Referenced, 0);
        row.status = Status::Ready;
        row.reviewed_at = Some(99);
        row.finding_count = Some(1);
        h.queue.put(&row).unwrap();

        let reply = h.map(&json!({ "handle": "r1" })).unwrap();
        let frames = reply["frames"].as_array().unwrap();
        assert_eq!(frames.len(), 2);

        let masked = frames[0]["file"].as_str().unwrap();
        assert!(masked.contains("redacted"), "served the original: {masked}");
        assert_eq!(frames[0]["redacted"], true);
        // The frame nobody masked is served as it is, and says so.
        assert!(!frames[1]["file"].as_str().unwrap().contains("redacted"));
        assert_eq!(frames[1]["redacted"], false);
        assert_eq!(reply["review"]["hidden"], 1);
        assert_eq!(reply["review"]["found"], 1);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn an_unreviewed_recording_gives_up_no_frames_at_all() {
        // The gate runs before any of the above. A recording waiting on a
        // person must not leak a single path, redacted or otherwise.
        let (h, root) = fixture("bridgegate");
        let cache = h.queue.cache_dir("r1");
        std::fs::create_dir_all(&cache).unwrap();
        let f = cache.join("frame-00001.webp");
        std::fs::write(&f, b"unreviewed").unwrap();
        std::fs::write(
            cache.join("frames.json"),
            serde_json::json!({ "frames": [{ "pts_time": 0.0, "file": f }] }).to_string(),
        )
        .unwrap();

        let mut row = Recording::new("r1", "C:/v/r1.mp4", Origin::Referenced, 0);
        row.status = Status::NeedsReview;
        row.finding_count = Some(3);
        h.queue.put(&row).unwrap();

        for call in [
            h.map(&json!({ "handle": "r1" })),
            h.frames(&json!({ "handle": "r1" })),
        ] {
            let (code, message) = call.unwrap_err();
            assert_eq!(code, ErrorCode::AwaitingReview);
            assert!(message.contains("waiting for your review"), "{message}");
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_path_that_was_never_queued_is_not_found_so_the_adapter_can_fall_back() {
        let (h, root) = fixture("bridgepath");
        let stray = root.join("never-queued.mp4");
        std::fs::write(&stray, b"a video nobody pasted").unwrap();
        let (code, _) = h.map(&json!({ "path": stray })).unwrap_err();
        assert_eq!(
            code,
            ErrorCode::NotFound,
            "the adapter reads this code to mean `read it yourself and say review was skipped`"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_time_range_narrows_the_frames_and_an_empty_one_is_not_an_error() {
        let (h, root) = fixture("bridgerange");
        let cache = h.queue.cache_dir("r1");
        std::fs::create_dir_all(&cache).unwrap();
        let mut list = Vec::new();
        for (i, t) in [0.0, 5.0, 10.0, 20.0].iter().enumerate() {
            let f = cache.join(format!("frame-{i}.webp"));
            std::fs::write(&f, b"x").unwrap();
            list.push(serde_json::json!({ "pts_time": t, "file": f }));
        }
        std::fs::write(
            cache.join("frames.json"),
            serde_json::json!({ "frames": list }).to_string(),
        )
        .unwrap();
        let mut row = Recording::new("r1", "C:/v/r1.mp4", Origin::Referenced, 0);
        row.status = Status::Ready;
        h.queue.put(&row).unwrap();

        let picked = h
            .frames(&json!({ "handle": "r1", "from_seconds": 4.0, "to_seconds": 11.0 }))
            .unwrap();
        assert_eq!(picked["frames_in_range"], 2);
        assert_eq!(picked["frames_total"], 4);

        // A quiet stretch is a boring answer, not a failure.
        let none = h
            .frames(&json!({ "handle": "r1", "from_seconds": 60.0 }))
            .unwrap();
        assert_eq!(none["frames_in_range"], 0);

        // Backwards bounds are a caller mistake and are named as one.
        let (code, _) = h
            .frames(&json!({ "handle": "r1", "from_seconds": 9.0, "to_seconds": 2.0 }))
            .unwrap_err();
        assert_eq!(code, ErrorCode::BadRequest);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn capabilities_claim_only_what_this_build_does() {
        let (h, _root) = fixture("caps");
        let mut s = Session::new(Box::new(h));
        let reply: Value = serde_json::from_str(
            &s.handle_line(
                br#"{"id":"0","method":"hello","params":{"client":"framekeep-mcp","protocol":1}}"#,
            )
            .to_line(),
        )
        .unwrap();
        let caps = reply["result"]["capabilities"].as_array().unwrap();
        // Every entry has to be backed by a method that answers today. This
        // list gained `frames` and `redaction` on 18/08, when the bridge and
        // the review screen made both true; before that the test existed to
        // stop `redaction` being claimed early, and it did its job.
        assert_eq!(
            caps,
            &[
                json!("queue"),
                json!("ingest"),
                json!("frames"),
                json!("redaction")
            ]
        );

        // A capability that must NEVER appear, whatever gets built. Framekeep is
        // the eye, not the brain (`FOUNDATION.md`, principle I): the day this
        // list offers to interpret a recording, the product has become the
        // thing it was defined against. Asserted here because a capability
        // string is exactly how such a feature would first announce itself.
        assert!(!caps.contains(&json!("analysis")));
        assert!(!caps.contains(&json!("search")));
    }

    #[test]
    fn retention_runs_and_says_what_it_did() {
        let (h, root) = fixture("sweep");
        std::fs::create_dir_all(root.join("cache").join("litter")).unwrap();
        h.queue
            .put(&Recording::new(
                "old",
                "C:/v/old.mp4",
                Origin::Referenced,
                0,
            ))
            .unwrap();

        let day = 24 * 60 * 60;
        let said = run_retention(&h.queue, &h.retention, 8 * day).unwrap();
        assert!(said.contains("1 expired"), "{said}");
        assert!(said.contains("1 empty folders swept"), "{said}");
        // Nothing of the user's was removed: nobody has been asked yet.
        assert!(said.contains("0 recording files removed"), "{said}");
    }
}
