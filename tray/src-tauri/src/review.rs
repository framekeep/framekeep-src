//! The review record: what the scan found, what the person decided, and the
//! one path from `needs_review` to `ready`. S5.4.
//!
//! # Two files, each with one writer
//!
//! `~/.framekeep/cache/<handle>/scan.json` is core's scan output, saved verbatim
//! by the pipeline and never touched again -- it is evidence, and evidence that
//! gets rewritten stops being evidence (the fixture lesson from S5.1, same
//! day). `review.json` is the person's ticks, written only from the window.
//! One merge function reads both; nobody else composes them.
//!
//! # The person's list is the law
//!
//! `apply` paints exactly the boxes that are still ticked. It never re-scans:
//! a redactor that recomputed its own list would overrule an unticked false
//! positive as surely as it would overrule a dragged box (S5.6). Core's
//! `redact --scan` flag exists for pipelines with nobody in them, and the
//! window must never use it.
//!
//! # There is no quiet way out
//!
//! This module is the only code in the tray that moves a row out of
//! `needs_review`. Every exit runs the redaction of whatever is ticked and
//! checks core's read-back verification before the row flips -- approving
//! with everything unticked is a person's decision to hide nothing, and that
//! is the one "skip" that exists.

use crate::pipeline::Runner;
use crate::queue::{Queue, QueueError, Status};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Core's scan summary, in the fields the review needs. Extra fields in the
/// file are tolerated: this is a reader of evidence, not its owner.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ScanFile {
    pub engine: Engine,
    pub frames: Vec<ScanFrame>,
    pub detections_total: usize,
    pub unlocated_total: usize,
    pub unreadable_frames: usize,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Engine {
    pub available: bool,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ScanFrame {
    pub file: String,
    #[serde(default)]
    pub pts_time: Option<f64>,
    pub detections: Vec<Detection>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Detection {
    /// The badge label, exactly as core wrote it (`API key`, `Token`, …).
    pub kind: String,
    /// `sk-••••••4f2a`. The raw value never existed on this side of the
    /// process boundary, which is the strongest privacy property this screen
    /// has: the window cannot leak what it was never given.
    pub masked: String,
    pub boxes: Vec<BoxF>,
    pub located: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize, Serialize)]
pub struct BoxF {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// The person's decisions: one bool per detection in scan order, plus the
/// regions they drew themselves (S5.6), per frame.
///
/// Shape-checked against `scan.json` on every read. Ticks reset to all-ticked
/// on a mismatch -- that direction hides more. Extras are DROPPED on a
/// mismatch, the opposite direction, and deliberately: a drawn box is pinned
/// to a frame index, and after a re-scan the same index can be a different
/// picture. Painting the wrong part of the wrong frame is the one thing worse
/// than hiding less (S5.3), and the reset forces a fresh look anyway.
#[derive(Debug, Deserialize, Serialize)]
struct ReviewFile {
    ticks: Vec<Vec<bool>>,
    #[serde(default)]
    extras: Vec<Vec<BoxF>>,
}

pub fn scan_path(cache_dir: &Path) -> std::path::PathBuf {
    cache_dir.join("scan.json")
}

fn review_path(cache_dir: &Path) -> std::path::PathBuf {
    cache_dir.join("review.json")
}

/// Persist core's scan output, verbatim. Called by the pipeline, once.
pub fn save_scan(cache_dir: &Path, scan: &serde_json::Value) -> Result<(), String> {
    write_atomic(&scan_path(cache_dir), scan)
}

pub fn load_scan(cache_dir: &Path) -> Result<ScanFile, String> {
    let text = std::fs::read_to_string(scan_path(cache_dir))
        .map_err(|e| format!("No scan record for this recording ({e})."))?;
    serde_json::from_str(&text).map_err(|e| format!("The scan record is unreadable ({e})."))
}

/// The person's decisions: ticks default to everything approved -- the
/// scanner flagged it, so until a person says otherwise it is due to be
/// hidden -- and extras default to none.
pub fn load_decisions(cache_dir: &Path, scan: &ScanFile) -> (Vec<Vec<bool>>, Vec<Vec<BoxF>>) {
    let wanted: Vec<usize> = scan.frames.iter().map(|f| f.detections.len()).collect();
    let fresh_extras = || vec![Vec::new(); scan.frames.len()];
    if let Ok(text) = std::fs::read_to_string(review_path(cache_dir)) {
        if let Ok(file) = serde_json::from_str::<ReviewFile>(&text) {
            if file.ticks.iter().map(Vec::len).collect::<Vec<_>>() == wanted {
                let extras = if file.extras.len() == scan.frames.len() {
                    // A flat or negative box paints nothing or the wrong
                    // thing; neither survives loading.
                    file.extras
                        .into_iter()
                        .map(|f| f.into_iter().filter(|b| b.w > 0.0 && b.h > 0.0).collect())
                        .collect()
                } else {
                    fresh_extras()
                };
                return (file.ticks, extras);
            }
        }
    }
    (
        wanted.iter().map(|&n| vec![true; n]).collect(),
        fresh_extras(),
    )
}

/// Save the decisions. Refuses shapes that do not match the scan, because a
/// tick or a drawn box that lands on the wrong frame hides the wrong thing.
pub fn save_decisions(
    cache_dir: &Path,
    scan: &ScanFile,
    ticks: &[Vec<bool>],
    extras: &[Vec<BoxF>],
) -> Result<(), String> {
    let wanted: Vec<usize> = scan.frames.iter().map(|f| f.detections.len()).collect();
    if ticks.iter().map(Vec::len).collect::<Vec<_>>() != wanted || extras.len() != scan.frames.len()
    {
        return Err(
            "Those decisions don't line up with what was scanned. Reopen the review.".into(),
        );
    }
    write_atomic(
        &review_path(cache_dir),
        &ReviewFile {
            ticks: ticks.to_vec(),
            extras: extras.to_vec(),
        },
    )
}

/// What one redact call answered, in the field that decides success.
///
/// `boxes_black` and nothing else. The first real approval taught why: the
/// person had unticked two findings, core's OCR re-scan of the output found
/// them -- as it had to, they were left visible on purpose -- and a gate on
/// `still_detected` parked a legitimate approval with an error telling them
/// to report a bug. A re-scan cannot tell a regression from a decision;
/// pixels under the painted boxes can.
#[derive(Debug, Deserialize)]
struct RedactReply {
    verification: Verification,
}

#[derive(Debug, Deserialize)]
struct Verification {
    boxes_black: bool,
}

/// The receipt `apply` hands back for the window's status line.
#[derive(Debug, PartialEq, serde::Serialize)]
pub struct Applied {
    pub frames_redacted: usize,
    pub masks_applied: usize,
    /// Ticked, but with no box to paint -- the scanner saw it and could not
    /// place it. The person was shown these; counting them keeps "approved"
    /// from reading as "hidden".
    pub left_visible: usize,
}

/// Paint what is ticked, verify the paint, and only then let the row leave
/// `needs_review`.
pub fn apply(queue: &Queue, handle: &str, runner: &dyn Runner) -> Result<Applied, String> {
    let mut row = queue
        .get(handle)
        .map_err(db)?
        .ok_or_else(|| "That recording is no longer in the queue.".to_string())?;

    let cache_dir = queue.cache_dir(handle);
    let scan = load_scan(&cache_dir)?;
    let (ticks, extras) = load_decisions(&cache_dir, &scan);

    let mut done = Applied {
        frames_redacted: 0,
        masks_applied: 0,
        left_visible: 0,
    };

    for (i, (frame, frame_ticks)) in scan.frames.iter().zip(&ticks).enumerate() {
        let approved: Vec<&Detection> = frame
            .detections
            .iter()
            .zip(frame_ticks)
            .filter(|(_, &t)| t)
            .map(|(d, _)| d)
            .collect();

        done.left_visible += approved.iter().filter(|d| d.boxes.is_empty()).count();

        // Ticked detections, then whatever the person drew themselves --
        // their regions need no tick: drawing one IS the decision.
        let mut boxes: Vec<&BoxF> = approved.iter().flat_map(|d| &d.boxes).collect();
        if let Some(drawn) = extras.get(i) {
            boxes.extend(drawn.iter());
        }
        if boxes.is_empty() {
            continue; // Nothing on this frame has a place to paint.
        }

        let src = Path::new(&frame.file);
        let name = src
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "frame".into());
        // webp regardless of the source: lossless, and the one format the
        // whole pipeline already standardises on. Never lossy -- a smeared
        // black edge over text is still text.
        let dest = cache_dir.join("redacted").join(format!("{name}.webp"));

        let mut args: Vec<String> = vec![
            "redact".into(),
            frame.file.clone(),
            "--out".into(),
            dest.to_string_lossy().into_owned(),
        ];
        for b in &boxes {
            args.push("--box".into());
            // Floats verbatim. Core owns the outward rounding; a second copy
            // of that rule here is how one of them ends up rounding inward.
            args.push(format!("{},{},{},{}", b.x, b.y, b.w, b.h));
        }

        let reply: RedactReply = serde_json::from_str(&runner.run(&args)?)
            .map_err(|e| format!("framekeep-core answered something unreadable ({e})."))?;

        // Painting is not proof; core reads the pixels back. Only the painted
        // boxes are checked -- what the person left visible is theirs to leave.
        if !reply.verification.boxes_black {
            return Err(format!(
                "The redaction did not fully cover an item on {}. \
                 Nothing was approved -- please report this.",
                src.file_name()
                    .map(|n| n.to_string_lossy())
                    .unwrap_or_default()
            ));
        }

        done.frames_redacted += 1;
        done.masks_applied += boxes.len();
    }

    row.status = Status::Ready;
    row.reviewed_at = Some(crate::queue::now_unix());
    queue.put(&row).map_err(db)?;
    Ok(done)
}

/// `.partial` then rename -- the discipline every store in this codebase uses,
/// for the same reason: nothing half-written may carry a finished name.
fn write_atomic<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let partial = path.with_extension("json.partial");
    let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    std::fs::write(&partial, text).map_err(|e| e.to_string())?;
    std::fs::rename(&partial, path).map_err(|e| e.to_string())?;
    Ok(())
}

fn db(e: QueueError) -> String {
    e.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::queue::Recording;
    use crate::retention::Origin;
    use std::sync::Mutex;

    fn fixture(name: &str) -> (Queue, std::path::PathBuf) {
        let root =
            std::env::temp_dir().join(format!("framekeep-review-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let queue = Queue::open_at(root.join("queue.db"), root.join("cache")).unwrap();
        (queue, root)
    }

    fn needs_review(queue: &Queue, handle: &str) {
        let mut r = Recording::new(handle, format!("C:/v/{handle}.mp4"), Origin::Referenced, 0);
        r.status = Status::NeedsReview;
        r.finding_count = Some(3);
        queue.put(&r).unwrap();
    }

    /// Two frames: one carries a located key and an unlocated token, one a
    /// located email. The shape every test here reads from.
    fn scan_fixture() -> serde_json::Value {
        serde_json::json!({
            "engine": { "available": true, "language": "en-US" },
            "frames": [
                {
                    "file": "C:/cache/h/frame-00001.webp",
                    "pts_time": 0.0,
                    "words": 40,
                    "detections": [
                        { "kind": "API key", "masked": "sk-••••••W4aZ", "located": true,
                          "boxes": [ { "x": 363.5, "y": 81.0, "w": 332.2, "h": 14.0 } ] },
                        { "kind": "Token", "masked": "ghp••••••ZeaE", "located": false,
                          "boxes": [] }
                    ]
                },
                {
                    "file": "C:/cache/h/frame-00002.webp",
                    "pts_time": 5.0,
                    "words": 12,
                    "detections": [
                        { "kind": "Email address", "masked": "qu••@acme.vn", "located": true,
                          "boxes": [ { "x": 100.0, "y": 200.0, "w": 180.0, "h": 15.0 },
                                     { "x": 300.0, "y": 200.0, "w": 40.0, "h": 15.0 } ] }
                    ]
                }
            ],
            "detections_total": 3,
            "unlocated_total": 1,
            "unreadable_frames": 0,
            "seconds": 0.1
        })
    }

    /// Records every redact call. `boxes_black` is what the gate reads;
    /// `still_detected` is carried to prove the gate IGNORES it -- the first
    /// real approval failed because it did not.
    struct Recorder {
        calls: Mutex<Vec<Vec<String>>>,
        boxes_black: bool,
        still_detected: usize,
    }

    impl Recorder {
        fn clean() -> Recorder {
            Recorder {
                calls: Mutex::new(Vec::new()),
                boxes_black: true,
                still_detected: 0,
            }
        }
    }

    impl Runner for Recorder {
        fn run(&self, args: &[String]) -> Result<String, String> {
            assert_eq!(args[0], "redact", "the review may only ever ask for redact");
            assert!(
                !args.contains(&"--scan".to_string()),
                "the window must never use --scan: the person's list is the law"
            );
            self.calls.lock().unwrap().push(args.to_vec());
            Ok(format!(
                r#"{{"verification":{{"boxes_black":{},"ran":true,"still_detected":{}}}}}"#,
                self.boxes_black, self.still_detected
            ))
        }
    }

    #[test]
    fn everything_ticked_paints_every_located_box_and_counts_the_rest() {
        let (queue, root) = fixture("all");
        needs_review(&queue, "h");
        let cache = queue.cache_dir("h");
        save_scan(&cache, &scan_fixture()).unwrap();

        let recorder = Recorder::clean();
        let done = apply(&queue, "h", &recorder).unwrap();

        assert_eq!(done.frames_redacted, 2);
        assert_eq!(done.masks_applied, 3, "one box on frame 1, two on frame 2");
        assert_eq!(done.left_visible, 1, "the unlocated token stays visible");

        let calls = recorder.calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        // Boxes travel verbatim, floats included -- core owns the rounding.
        assert!(calls[0].contains(&"363.5,81,332.2,14".to_string()));
        // Output lands under redacted/, never over the original.
        assert!(calls[0].iter().any(|a| a.contains("redacted")));

        let row = queue.get("h").unwrap().unwrap();
        assert_eq!(row.status, Status::Ready);
        assert!(row.reviewed_at.is_some(), "the approval must leave a date");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn an_unticked_finding_is_not_painted_because_the_person_said_so() {
        let (queue, root) = fixture("untick");
        needs_review(&queue, "h");
        let cache = queue.cache_dir("h");
        let scan: ScanFile = serde_json::from_value(scan_fixture()).unwrap();
        save_scan(&cache, &scan_fixture()).unwrap();
        // Untick the API key on frame 1; keep everything else.
        save_decisions(
            &cache,
            &scan,
            &[vec![false, true], vec![true]],
            &[vec![], vec![]],
        )
        .unwrap();

        let recorder = Recorder::clean();
        let done = apply(&queue, "h", &recorder).unwrap();

        // Frame 1 had only the unticked key with boxes left, so no call for it.
        assert_eq!(done.frames_redacted, 1);
        let calls = recorder.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert!(
            !calls[0].iter().any(|a| a.contains("363.5")),
            "an unticked box was painted -- the person's decision was overruled"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn everything_unticked_is_an_approval_to_hide_nothing() {
        let (queue, root) = fixture("none");
        needs_review(&queue, "h");
        let cache = queue.cache_dir("h");
        let scan: ScanFile = serde_json::from_value(scan_fixture()).unwrap();
        save_scan(&cache, &scan_fixture()).unwrap();
        save_decisions(
            &cache,
            &scan,
            &[vec![false, false], vec![false]],
            &[vec![], vec![]],
        )
        .unwrap();

        let recorder = Recorder::clean();
        let done = apply(&queue, "h", &recorder).unwrap();

        assert_eq!(done.frames_redacted, 0);
        assert!(recorder.calls.lock().unwrap().is_empty());
        // Still a review: a person looked at every finding and chose. The gate
        // exists to guarantee the looking, not to force the hiding.
        assert_eq!(queue.get("h").unwrap().unwrap().status, Status::Ready);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_failed_paint_keeps_the_row_in_review() {
        let (queue, root) = fixture("verify");
        needs_review(&queue, "h");
        let cache = queue.cache_dir("h");
        save_scan(&cache, &scan_fixture()).unwrap();

        let recorder = Recorder {
            calls: Mutex::new(Vec::new()),
            boxes_black: false,
            still_detected: 0,
        };
        let err = apply(&queue, "h", &recorder).unwrap_err();
        assert!(err.contains("did not fully cover"), "{err}");

        // The row must not have moved: a paint that did not take is not an
        // approval, and ready-with-visible-secrets is the worst outcome.
        assert_eq!(queue.get("h").unwrap().unwrap().status, Status::NeedsReview);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn what_the_person_left_visible_does_not_fail_their_approval() {
        // The first real click found this. Two findings were deliberately
        // unticked; the OCR re-scan of the output saw them -- as it must --
        // and a gate on still_detected parked the approval with "please
        // report this". The scanner cannot tell a regression from a decision,
        // so the gate may not read it.
        let (queue, root) = fixture("leftvisible");
        needs_review(&queue, "h");
        let cache = queue.cache_dir("h");
        let scan: ScanFile = serde_json::from_value(scan_fixture()).unwrap();
        save_scan(&cache, &scan_fixture()).unwrap();
        save_decisions(
            &cache,
            &scan,
            &[vec![true, false], vec![false]],
            &[vec![], vec![]],
        )
        .unwrap();

        let recorder = Recorder {
            calls: Mutex::new(Vec::new()),
            boxes_black: true,
            still_detected: 3, // everything unticked, still readable, on purpose
        };
        let done = apply(&queue, "h", &recorder).unwrap();
        assert_eq!(done.frames_redacted, 1);
        assert_eq!(
            queue.get("h").unwrap().unwrap().status,
            Status::Ready,
            "a person's decision to leave items visible is not a paint failure"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_drawn_region_is_painted_without_needing_a_tick() {
        let (queue, root) = fixture("extras");
        needs_review(&queue, "h");
        let cache = queue.cache_dir("h");
        let scan: ScanFile = serde_json::from_value(scan_fixture()).unwrap();
        save_scan(&cache, &scan_fixture()).unwrap();
        // Untick EVERYTHING the scanner found; draw one region on frame 2.
        save_decisions(
            &cache,
            &scan,
            &[vec![false, false], vec![false]],
            &[
                vec![],
                vec![BoxF {
                    x: 40.5,
                    y: 60.0,
                    w: 120.0,
                    h: 30.0,
                }],
            ],
        )
        .unwrap();

        let recorder = Recorder::clean();
        let done = apply(&queue, "h", &recorder).unwrap();

        assert_eq!(done.frames_redacted, 1, "only the frame with the drawing");
        assert_eq!(done.masks_applied, 1);
        let calls = recorder.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert!(
            calls[0].contains(&"40.5,60,120,30".to_string()),
            "the drawn box travels verbatim: {:?}",
            calls[0]
        );
        assert!(
            calls[0][1].ends_with("frame-00002.webp"),
            "the drawing must land on ITS frame, not the first one"
        );
        assert_eq!(queue.get("h").unwrap().unwrap().status, Status::Ready);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn stale_extras_from_a_different_scan_are_dropped_not_misapplied() {
        // Ticks reset toward hiding more; extras are dropped instead, because
        // a drawn box pinned to frame index 2 of an OLD scan can sit on a
        // completely different picture in the new one. Painting the wrong
        // part of the wrong frame is worse than hiding less.
        let (_queue, root) = fixture("staleextras");
        let cache = root.join("cache").join("h");
        std::fs::create_dir_all(&cache).unwrap();
        save_scan(&cache, &scan_fixture()).unwrap();
        let scan: ScanFile = serde_json::from_value(scan_fixture()).unwrap();

        std::fs::write(
            cache.join("review.json"),
            r#"{"ticks":[[true],[true],[true]],"extras":[[{"x":1,"y":2,"w":3,"h":4}],[],[]]}"#,
        )
        .unwrap();

        let (_, extras) = load_decisions(&cache, &scan);
        assert_eq!(
            extras,
            vec![Vec::<BoxF>::new(), Vec::new()],
            "extras pinned to a different scan's frames must not survive"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn ticks_from_a_different_scan_shape_reset_to_all_hidden() {
        let (_queue, root) = fixture("shape");
        let cache = root.join("cache").join("h");
        std::fs::create_dir_all(&cache).unwrap();
        save_scan(&cache, &scan_fixture()).unwrap();
        let scan: ScanFile = serde_json::from_value(scan_fixture()).unwrap();

        // A stale review.json from before a re-scan: wrong shape.
        std::fs::write(
            cache.join("review.json"),
            r#"{"ticks":[[false],[false],[false]]}"#,
        )
        .unwrap();

        let (ticks, extras) = load_decisions(&cache, &scan);
        assert_eq!(
            ticks,
            vec![vec![true, true], vec![true]],
            "a mismatched shape must reset toward hiding, never toward showing"
        );
        assert_eq!(extras, vec![Vec::<BoxF>::new(), Vec::new()]);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn saving_ticks_with_the_wrong_shape_is_refused() {
        let (_queue, root) = fixture("badsave");
        let cache = root.join("cache").join("h");
        std::fs::create_dir_all(&cache).unwrap();
        save_scan(&cache, &scan_fixture()).unwrap();
        let scan: ScanFile = serde_json::from_value(scan_fixture()).unwrap();

        assert!(save_decisions(&cache, &scan, &[vec![true]], &[vec![], vec![]]).is_err());
        // And an extras list that does not cover every frame is refused too.
        assert!(save_decisions(&cache, &scan, &[vec![true, true], vec![true]], &[vec![]]).is_err());
        let _ = std::fs::remove_dir_all(root);
    }
}
