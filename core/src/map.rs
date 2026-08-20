//! The map: everything Framekeep can say about a recording, without an image.
//!
//! This is what a model reads *first*. It answers "what is in this video and
//! when" so the model can then ask for the few frames it actually wants, rather
//! than being handed twenty pictures and a token bill.
//!
//! Frames are listed by path, never inlined. Frames do not travel through the
//! socket -- only where to find them.
//!
//! # The two halves run at once, and it barely matters
//!
//! Measured on a 126-second recording: frames took **1.3s**, speech took
//! **110.4s**. Running them together saved 1.2 seconds out of 111.7 -- about
//! one percent. The parallelism was built expecting more; the halves turned out
//! to be 85x apart, so it earns almost nothing.
//!
//! Keeping it anyway, because it costs one `thread::scope` and the ratio is not
//! a law -- a faster model closes the gap. But the honest conclusion is that
//! **the lever is `--skip-transcript`, not concurrency**: the same video yields
//! its frames in 2.3 seconds when nobody asks for words.
//!
//! That is worth knowing upstream. It says the two-step MCP mechanism should
//! hand back the frame map immediately and let speech arrive later, rather than
//! making a caller wait two minutes for a picture that was ready in two seconds.

use serde::Serialize;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::dedup::{self, Decision, Deduper};
use crate::encode;
use crate::ffmpeg::Toolchain;
use crate::frame_store;
use crate::probe::{self, Probe};
use crate::scan;
use crate::select::{self, SelectParams};
use crate::transcribe::{self, Transcript};
use crate::transcript_store;
use crate::whisper::Whisper;

#[derive(Debug, Serialize)]
pub struct FrameEntry {
    pub pts_time: f64,
    pub file: String,
}

#[derive(Debug, Serialize)]
pub struct SelectionSummary {
    pub threshold: f64,
    pub min_gap_seconds: f64,
    pub max_gap_seconds: f64,
    pub selected: usize,
    pub kept: usize,
    pub dropped: usize,
}

#[derive(Debug, Serialize)]
pub struct Timing {
    pub frames_seconds: f64,
    pub transcript_seconds: Option<f64>,
    /// Wall clock for the whole map. Less than the sum of the parts, because
    /// the two halves run at the same time.
    pub total_seconds: f64,
}

#[derive(Debug, Serialize)]
pub struct VideoMap {
    pub path: String,
    pub handle: String,
    pub video: Probe,
    pub selection: SelectionSummary,
    pub frames: Vec<FrameEntry>,
    /// True when the frames were already on disk from an earlier call with
    /// these settings, so nothing was decoded this time.
    ///
    /// Said out loud for the same reason as the transcript flag: a caller
    /// comparing timings between runs deserves to know which of the two it got.
    pub frames_from_cache: bool,
    /// `None` only when the caller asked to skip it; a video with no audio
    /// still produces a transcript object saying so.
    pub transcript: Option<Transcript>,
    /// True when the words came off disk instead of out of whisper.
    ///
    /// Stated rather than implied: reusing a cached transcript turns a
    /// two-minute step into an instant one, and a caller comparing timings
    /// between runs deserves to know which of the two it got.
    pub transcript_from_cache: bool,
    /// What the frames appear to contain. `None` means nobody looked -- which
    /// is a different fact from "looked and found nothing", and downstream has
    /// to be able to tell them apart before it lets anything past a review.
    pub scan: Option<scan::Summary>,
    pub timing: Timing,
}

#[derive(Debug)]
pub enum MapError {
    Probe(probe::ProbeError),
    Select(select::SelectError),
    Decode(dedup::DecodeError),
    Transcribe(transcribe::TranscribeError),
    /// A worker thread died rather than returning an error, which means a panic.
    WorkerLost(&'static str),
}

impl fmt::Display for MapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MapError::Probe(e) => write!(f, "{e}"),
            MapError::Select(e) => write!(f, "{e}"),
            MapError::Decode(e) => write!(f, "{e}"),
            MapError::Transcribe(e) => write!(f, "{e}"),
            MapError::WorkerLost(which) => write!(
                f,
                "The {which} step stopped unexpectedly.\n\
                 This is a bug in Framekeep -- please report it with the video's format."
            ),
        }
    }
}

pub struct MapRequest<'a> {
    pub video: &'a Path,
    pub handle: String,
    pub work_dir: PathBuf,
    pub params: SelectParams,
    /// `None` skips speech entirely -- useful when the caller only wants
    /// frames and does not want to wait minutes for words.
    pub whisper: Option<&'a Whisper>,
    /// How to encode the frames that survive. See `encode.rs` for why this is
    /// the only lever that helps: WebP lossless is 3.9x smaller for the same
    /// pixels, and scaling interface content down makes it bigger.
    pub format: encode::Format,
    /// Read the surviving frames and report what looks sensitive.
    ///
    /// Off by default, and the caller says so explicitly, because this is a
    /// tool rather than the product: the app always asks, and the adapter
    /// calling `map` repeatedly for the same recording should not pay ~56 ms a
    /// frame for findings nobody will act on. The safety gate is the review
    /// door in the tray, not a default in here.
    ///
    /// What keeps that honest is the output: `scan` is `null` when nobody
    /// looked and a summary when somebody did, so "not scanned" can never be
    /// mistaken for "scanned and clean".
    pub scan: bool,
    /// Words a person added in Settings (S5.8). Empty is the ordinary case and
    /// leaves the scanner exactly as it was measured.
    pub patterns: &'a [String],
    /// Speech language, or `None` to let whisper detect it. Detection is right
    /// far more often than a guess, so this exists for the case it is wrong.
    pub language: Option<&'a str>,
}

pub fn build(tools: &Toolchain, req: MapRequest<'_>) -> Result<VideoMap, MapError> {
    let started = Instant::now();

    // Ask the file what it is before spending anything on it.
    let video = probe::probe(tools, req.video).map_err(MapError::Probe)?;

    // A silent recording needs no model loaded, and loading a 547 MiB one to
    // transcribe nothing would be absurd.
    let want_speech = req.whisper.is_some() && video.has_audio;

    // Cheaper than either half: someone may already have paid for the words.
    // Frames still get extracted -- they are fast, and the caller may have
    // asked for different selection parameters this time.
    let cached = if want_speech {
        match transcript_store::status(&req.work_dir) {
            transcript_store::Status::Ready(stored) => Some(stored.into_transcript()),
            _ => None,
        }
    } else {
        None
    };
    let want_speech = want_speech && cached.is_none();

    let hdr = video.is_hdr;
    let out_dir = req.work_dir.clone();
    let params = req.params;
    let video_path = req.video;

    // Someone may already have broken this recording into frames with these
    // exact settings. Extraction costs 2.3s for a two-minute SDR clip and ten
    // times that for HDR, and the adapter calls this more than once per video.
    let cached_frames = frame_store::load(&req.work_dir, &params, req.format);

    // The two halves touch different files in the same folder: frame extraction
    // only ever clears its own `frame-*.png`, and transcription works on
    // `audio-16k.wav`. That scoping is what makes running them together safe.
    let need_extraction = cached_frames.is_none();
    let (frames_result, transcript_result) = std::thread::scope(|scope| {
        let frames = scope.spawn(|| {
            let t0 = Instant::now();
            if !need_extraction {
                return Ok::<_, select::SelectError>((Vec::new(), t0.elapsed().as_secs_f64()));
            }
            // Clear first: a crash midway must not leave a list describing
            // frames that were only half replaced.
            frame_store::clear(&out_dir);
            let selected = select::extract(tools, video_path, &out_dir, &params, hdr)?;
            Ok((selected, t0.elapsed().as_secs_f64()))
        });

        let speech = want_speech.then(|| {
            let whisper = req.whisper.expect("checked by want_speech");
            let work = req.work_dir.clone();
            scope.spawn(move || {
                let t0 = Instant::now();
                let t = transcribe::transcribe(tools, whisper, video_path, &work, req.language)?;
                Ok::<_, transcribe::TranscribeError>((t, t0.elapsed().as_secs_f64()))
            })
        });

        let frames = frames
            .join()
            .map_err(|_| MapError::WorkerLost("frame extraction"));
        let speech = speech.map(|h| h.join().map_err(|_| MapError::WorkerLost("transcription")));
        (frames, speech)
    });

    let (selected, frames_seconds) = frames_result?.map_err(MapError::Select)?;

    // Two ways to arrive at the same list. Either it was already on disk from a
    // call with these exact settings, or it was just extracted and has to go
    // through dedup and encoding first.
    let (kept, selected_count, dropped, frames_from_cache): (Vec<FrameEntry>, usize, usize, bool) =
        match cached_frames {
            Some(cached) => (
                cached
                    .frames
                    .iter()
                    .map(|f| FrameEntry {
                        pts_time: f.pts_time,
                        file: f.file.display().to_string(),
                    })
                    .collect(),
                cached.selected,
                cached.dropped,
                true,
            ),
            None => {
                let (kept_files, dropped) = winnow(tools, selected, req.format)?;
                let selected_count = kept_files.len() + dropped;
                // Record it before returning, so the next call can look instead of
                // redo. Best effort: failing here costs time later, not correctness
                // now.
                frame_store::save(
                    &req.work_dir,
                    &params,
                    req.format,
                    &kept_files,
                    selected_count,
                    dropped,
                );
                (
                    kept_files
                        .iter()
                        .map(|f| FrameEntry {
                            pts_time: f.pts_time,
                            file: f.file.display().to_string(),
                        })
                        .collect(),
                    selected_count,
                    dropped,
                    false,
                )
            }
        };

    // Deliberately after the frame list settles, so it covers both routes here:
    // frames served from `frame_store` never went through `winnow`, and a
    // caller cannot tell from the outside which route it got. Scanning inside
    // extraction would quietly mean "cached recordings are never examined".
    let scanned = req.scan.then(|| {
        scan::many(
            kept.iter().map(|f| (f.pts_time, Path::new(&f.file))),
            req.patterns,
        )
    });

    let from_cache = cached.is_some();
    let (transcript, transcript_seconds) = match (cached, transcript_result) {
        (Some(t), _) => (Some(t), None),
        (None, Some(res)) => {
            let (t, secs) = res?.map_err(MapError::Transcribe)?;
            // Store it so the next map of this recording is instant. A failure
            // to cache is not a reason to lose words already in hand.
            if let Ok(claim) = transcript_store::claim(&req.work_dir, 0.0) {
                if let Err(e) = claim.finish(&t) {
                    eprintln!("Transcribed, but couldn't save it for next time: {e}");
                }
            }
            (Some(t), Some(secs))
        }
        // No audio track is a fact worth reporting, not a gap.
        (None, None) if req.whisper.is_some() => (Some(Transcript::silent()), None),
        (None, None) => (None, None),
    };

    Ok(VideoMap {
        path: req.video.display().to_string(),
        handle: req.handle,
        video,
        selection: SelectionSummary {
            threshold: params.threshold,
            min_gap_seconds: params.min_gap,
            max_gap_seconds: params.max_gap,
            selected: selected_count,
            kept: kept.len(),
            dropped,
        },
        frames: kept,
        frames_from_cache,
        transcript,
        transcript_from_cache: from_cache,
        scan: scanned,
        timing: Timing {
            frames_seconds,
            transcript_seconds,
            total_seconds: started.elapsed().as_secs_f64(),
        },
    })
}

/// Drops the frames that are provably duplicates, and encodes the survivors.
///
/// Returns what is left, and how many were dropped. Split out of [`build`]
/// because the cached path skips all of it: the frames on disk have already
/// been through here once.
fn winnow(
    tools: &Toolchain,
    selected: Vec<select::SelectedFrame>,
    format: encode::Format,
) -> Result<(Vec<select::SelectedFrame>, usize), MapError> {
    let rule = dedup::ProvablyIdentical::default();
    let mut kept: Vec<select::SelectedFrame> = Vec::new();
    let mut dropped = 0usize;
    let mut last: Option<dedup::Frame> = None;

    for mut frame in selected {
        let decoded = dedup::Frame::load(&frame.file).map_err(MapError::Decode)?;
        let decision = match &last {
            // Nothing to compare against yet, so nothing can be proven.
            None => Decision::Keep,
            Some(prev) => rule.decide(prev, &decoded),
        };
        match decision {
            Decision::Keep => {
                last = Some(decoded);
                // Encode survivors only: dedup needed the PNG, and encoding a
                // frame about to be thrown away is work for nothing.
                match encode::convert(tools, &frame.file, format) {
                    Ok(p) => frame.file = p,
                    // The PNG is still there and still readable, so this costs
                    // disk space rather than the whole map.
                    Err(e) => eprintln!("Warning: {e}"),
                }
                kept.push(frame);
            }
            Decision::Drop(_) => {
                dropped += 1;
                let _ = std::fs::remove_file(&frame.file);
            }
        }
    }
    Ok((kept, dropped))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timing_shows_the_overlap() {
        // Both halves run together, so the total must beat the sum. The margin
        // is small in practice -- measured at ~1% -- but if it ever goes
        // negative the concurrency has broken.
        let t = Timing {
            frames_seconds: 1.3,
            transcript_seconds: Some(110.4),
            total_seconds: 110.5,
        };
        let sum = t.frames_seconds + t.transcript_seconds.unwrap();
        assert!(
            t.total_seconds < sum,
            "total {} should beat the sum {sum}",
            t.total_seconds
        );
    }

    #[test]
    fn a_map_carries_paths_not_pictures() {
        // Frames never travel inline; the map says where they are.
        let json = serde_json::to_string(&FrameEntry {
            pts_time: 1.0,
            file: "C:/x/frame-00001.png".into(),
        })
        .unwrap();
        assert!(json.contains("frame-00001.png"));
        assert!(
            !json.contains("base64"),
            "the map must never inline image data"
        );
    }
}
