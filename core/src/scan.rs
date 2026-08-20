//! Frames in, findings out. S5.1.
//!
//! One place where `ocr` and `secrets` meet, so the CLI's `scan` command and the
//! frame pipeline cannot drift into two different answers for the same picture.
//!
//! # What this reports, and what it refuses to report
//!
//! Never the secret. A finding carries `sk-••••••4f2a` and a box, which is
//! enough for a person to recognise which key it is and enough for `redact.rs`
//! to cover it, and not enough for anyone reading a log to use it. A tool built
//! to hide secrets must not be the thing that writes one down.
//!
//! # Two counts that exist to stop a silent pass
//!
//! `unlocated` is a detection with no box to cover -- something is there and
//! nothing can be painted over it. `unreadable` is a frame that never produced
//! a reading at all. Both default to zero and both are worth more than the
//! findings list: a caller that treats "no findings" as "safe to send" is right
//! only when both are zero, and neither is visible unless it is counted.
//!
//! The whole scan is optional at the CLI, so callers also have to tell apart
//! "nobody looked" from "looked and found nothing". That is why the map's field
//! is `null` when unasked rather than an empty summary.

use serde::Serialize;
use std::path::Path;
use std::time::Instant;

use crate::ocr;
use crate::secrets;

#[derive(Debug, Serialize)]
pub struct Detection {
    /// The badge a person sees. From `secrets::Kind::label`.
    pub kind: &'static str,
    /// `sk-••••••4f2a` -- which key, never the key.
    pub masked: String,
    /// One rect per word, never a union. See `ocr::Reading::boxes_for`.
    pub boxes: Vec<ocr::Rect>,
    /// False when the pattern matched but no box lines up with it, so there is
    /// nothing to paint over. Measured as rare, never assumed impossible.
    pub located: bool,
}

#[derive(Debug, Serialize)]
pub struct FrameScan {
    pub file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pts_time: Option<f64>,
    pub words: usize,
    pub detections: Vec<Detection>,
    /// Why this frame produced nothing. Present only when it failed -- a frame
    /// that could not be read is not a clean frame, and the two must not look
    /// alike in the output.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Engine {
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Summary {
    pub engine: Engine,
    pub frames: Vec<FrameScan>,
    pub detections_total: usize,
    /// Detections with nothing to cover them. Non-zero means redaction cannot
    /// be complete on this recording no matter what happens downstream.
    pub unlocated_total: usize,
    /// Frames that produced no reading. Same warning, different cause.
    pub unreadable_frames: usize,
    pub seconds: f64,
}

/// Scan one image file.
///
/// `patterns` are the words a person added in Settings (S5.8). They arrive as
/// an argument all the way from the command line -- nothing in this crate goes
/// looking for the app's settings file, so the same frame and the same flags
/// give the same answer on any machine.
fn read(path: &Path, patterns: &[String]) -> Result<(ocr::Reading, Vec<Detection>), ocr::Error> {
    let bytes = std::fs::read(path)
        .map_err(|e| ocr::Error::Failed(format!("cannot read {}: {e}", path.display())))?;
    let reading = ocr::read(&bytes)?;
    // The composition lives here rather than on `Reading`: `ocr` has no reason
    // to know what a secret is, and a module that reads pixels should not gain
    // an opinion about API keys.
    let detections = secrets::scan_with(&reading.text, patterns)
        .into_iter()
        .map(|finding| {
            let boxes = reading.boxes_for(&finding.span);
            Detection {
                kind: finding.kind.label(),
                masked: finding.masked(),
                located: !boxes.is_empty(),
                boxes,
            }
        })
        .collect();
    Ok((reading, detections))
}

/// Scan a single image, for `framekeep-core scan`.
pub fn one(path: &Path, patterns: &[String]) -> Result<(String, FrameScan), ocr::Error> {
    let (reading, detections) = read(path, patterns)?;
    Ok((
        reading.language.clone(),
        FrameScan {
            file: path.display().to_string(),
            pts_time: None,
            words: reading.words.len(),
            detections,
            error: None,
        },
    ))
}

/// Scan every frame of a recording.
///
/// Stops on the first `NoEngine`: that answer is about the machine, not the
/// file, so the remaining frames would each spend a WinRT activation to be told
/// the same thing. A per-file failure does not stop anything -- it is recorded
/// against that frame and the scan carries on.
pub fn many<'a>(frames: impl IntoIterator<Item = (f64, &'a Path)>, patterns: &[String]) -> Summary {
    let started = Instant::now();
    let mut out = Vec::new();
    let mut language = None;
    let mut engine_missing = None;

    for (pts_time, path) in frames {
        match read(path, patterns) {
            Ok((reading, detections)) => {
                if language.is_none() {
                    language = Some(reading.language.clone());
                }
                out.push(FrameScan {
                    file: path.display().to_string(),
                    pts_time: Some(pts_time),
                    words: reading.words.len(),
                    detections,
                    error: None,
                });
            }
            Err(ocr::Error::NoEngine(why)) => {
                engine_missing = Some(why);
                break;
            }
            Err(ocr::Error::Failed(why)) => out.push(FrameScan {
                file: path.display().to_string(),
                pts_time: Some(pts_time),
                words: 0,
                detections: Vec::new(),
                error: Some(why),
            }),
        }
    }

    let detections_total = out.iter().map(|f| f.detections.len()).sum();
    let unlocated_total = out
        .iter()
        .flat_map(|f| &f.detections)
        .filter(|d| !d.located)
        .count();
    let unreadable_frames = out.iter().filter(|f| f.error.is_some()).count();

    Summary {
        engine: Engine {
            available: engine_missing.is_none(),
            language: language.filter(|_| engine_missing.is_none()),
            reason: engine_missing,
        },
        frames: out,
        detections_total,
        unlocated_total,
        unreadable_frames,
        seconds: started.elapsed().as_secs_f64(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Every path below runs on a machine with no OCR engine reachable for these
    /// files, which is the point: the shape of a scan that found nothing must
    /// still say why, and must not read as "clean".
    #[test]
    fn a_missing_file_is_a_frame_level_failure_not_an_engine_verdict() {
        let missing = PathBuf::from("no-such-frame-9d2f.png");
        let s = many([(1.0, missing.as_path())], &[]);
        assert_eq!(s.unreadable_frames, 1);
        assert_eq!(s.detections_total, 0);
        assert!(
            s.frames[0].error.is_some(),
            "a frame that could not be read must say so"
        );
    }

    #[test]
    fn nothing_scanned_is_not_the_same_as_nothing_found() {
        // The distinction the map relies on: `scan: null` means nobody looked.
        // An empty summary means somebody looked and the frames were clean.
        let empty = many(std::iter::empty::<(f64, &Path)>(), &[]);
        assert_eq!(empty.detections_total, 0);
        assert!(
            serde_json::to_string(&Some(empty)).unwrap() != "null",
            "a summary must never serialise as the same thing as an unasked scan"
        );
    }

    #[test]
    fn a_summary_never_carries_the_secret_itself() {
        // Enforced at the type level -- `Detection` has no field for it -- and
        // pinned here so adding one is a test failure rather than a review miss.
        let json = serde_json::to_string(&Detection {
            kind: "API key",
            masked: "sk-••••••4f2a".into(),
            boxes: vec![ocr::Rect {
                x: 1.0,
                y: 2.0,
                w: 3.0,
                h: 4.0,
            }],
            located: true,
        })
        .unwrap();
        assert!(json.contains("sk-"));
        assert!(json.contains('•'), "the value must arrive masked");
        assert_eq!(json.matches("sk-").count(), 1);
    }
}
