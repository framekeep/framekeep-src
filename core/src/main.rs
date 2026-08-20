//! framekeep-core -- the only place in Framekeep that knows how to handle video.
//!
//! No UI, no IPC server. Runs as a CLI so it can be driven by the tray, by the
//! MCP adapter, or by a person debugging a file that will not process.
//!
//! Output is JSON on stdout; human-facing errors go to stderr. Anything reading
//! this programmatically can trust stdout to be JSON or empty.

mod cache;
mod dedup;
mod encode;
mod ffmpeg;
mod frame_store;
mod map;
mod models;
mod ocr;
mod probe;
mod redact;
mod scan;
mod secrets;
mod select;
mod transcribe;
mod transcript_store;
mod whisper;

use serde::Serialize;
use std::path::PathBuf;
use std::process::ExitCode;

use dedup::{Decision, Deduper};

const USAGE: &str = "\
framekeep-core -- video processing for Framekeep

USAGE:
  framekeep-core map <path>         Everything about a recording except the pictures
  framekeep-core probe <path>       Report what a video file is: duration, size, codec, fps
  framekeep-core frames <path>      Extract the frames the video is actually made of
  framekeep-core crop <path>        Crop the frames to a region, at original size
  framekeep-core transcribe <path>  Timestamped speech, as a guide to the frames
  framekeep-core transcript <path>  Is the speech ready yet? Reads, never starts work
  framekeep-core scan <image>       Read a frame's text and report what looks sensitive.
                                   Values come back masked -- this never prints a
                                   secret it found. Windows only for now.
  framekeep-core redact <image>     Paint rectangles black and write a new image.
                                   The source is never modified.
  framekeep-core models             What speech models exist, and which are installed
  framekeep-core models get <name>  Show the download size; add --yes to actually fetch it
  framekeep-core doctor             Check the bundled tools are present and match this build
  framekeep-core doctor --json      The same check, for a program instead of a person
  framekeep-core --version

MAP OPTIONS:
  --skip-transcript         Frames only. Speech is the slow half -- at the default
                            model's ~1x realtime, a two-minute video spends two
                            minutes on words the caller may not need.
  --model <path>            Use a specific model file instead of the installed one
  ...plus every FRAMES option below.

CROP OPTIONS:
  --region <x1,y1,x2,y2>    Rectangle in the frame's own pixels. Required.
                            Cropping is how a caller fits more frames into one
                            MCP reply: the cap counts pixel AREA, not bytes, and
                            a crop spends none of the pixels it keeps. Scaling
                            would buy the same room by blurring the text -- so
                            it is not offered.
  --format <png|webp>       Encoding for the cropped copies       (default webp)

REDACT OPTIONS:
  --out <path>              Where to write the redacted copy. Required -- this
                            never writes over the picture it read, because the
                            review screen shows a person that original.
                            The format follows this file's extension.
  --box <x,y,w,h>           A rectangle to cover, in the frame's own pixels.
                            Repeatable, and fractions are fine -- OCR word
                            boxes pass through verbatim; covering rounds
                            outward here, in one place. One box per word,
                            never a box around several: a token OCR split in
                            two sits at both ends of a line, and the space
                            between belongs to the user.
  --scan                    Also cover whatever a scan finds on its own. A
                            convenience for callers with nobody in the loop --
                            once a person reviews, THEY decide the list and it
                            arrives as --box.
                            Reports anything detected with no box to cover it,
                            because that is a secret this cannot hide.
                            Either way the result is read back and scanned
                            again: boxes in the right place and text actually
                            gone are two different claims.

TRANSCRIBE OPTIONS:
  --model <path>            Use a specific model file instead of the installed one
  --fresh                   Redo the work even if a transcript is already cached.
                            Cached results are reused by default: at the default
                            model's ~1x realtime, transcribing the same two-minute
                            recording twice costs two minutes for nothing.

FRAMES OPTIONS:
  --threshold <0.002-0.60>  Scene-change score worth keeping        (default 0.012)
                            Measured on real screen content: a full screen change
                            scores ~0.015, continuous scrolling ~0.078.
  --min-gap <seconds>       Quiet period after a kept frame         (default 5.0)
                            This, not the threshold, is what bounds scrolling.
  --max-gap <seconds>       Keep a frame even with no change after  (default 15.0)
  --out <dir>               Where to write frames (default ~/.framekeep/cache/<handle>)
  --format <png|webp>       Frame encoding                       (default png)
                            webp is LOSSLESS and 3.9x smaller on interface
                            content. It saves disk and I/O, not reply room:
                            an MCP client's cap counts pixel area, not bytes.
                            Never lossy: artefacts make a model misread the
                            text on screen.
  --keep-duplicates         Skip dedup and report what it would have dropped
  --scan                    Read the kept frames and report what looks sensitive:
                            masked values, plus a box per word to cover. Costs
                            about 56 ms a frame. Off by default because this is
                            a tool -- the app always asks. Without it the `scan`
                            field is null, which means NOBODY LOOKED; an empty
                            summary means somebody looked and found nothing.
                            Windows only (macOS uses Apple Vision, slice S7).

Output is JSON on stdout. Errors are plain text on stderr.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    match args.first().map(String::as_str) {
        Some("map") => match args.get(1) {
            Some(path) if !path.starts_with("--") => cmd_map(PathBuf::from(path), &args[2..]),
            _ => usage_error("map needs a file path."),
        },
        Some("probe") => match args.get(1) {
            Some(path) => cmd_probe(PathBuf::from(path)),
            None => usage_error("probe needs a file path."),
        },
        Some("frames") => match args.get(1) {
            Some(path) if !path.starts_with("--") => cmd_frames(PathBuf::from(path), &args[2..]),
            _ => usage_error("frames needs a file path."),
        },
        Some("crop") => match args.get(1) {
            Some(path) if !path.starts_with("--") => cmd_crop(PathBuf::from(path), &args[2..]),
            _ => usage_error("crop needs a file path."),
        },
        Some("transcribe") => match args.get(1) {
            Some(path) if !path.starts_with("--") => {
                cmd_transcribe(PathBuf::from(path), &args[2..])
            }
            _ => usage_error("transcribe needs a file path."),
        },
        Some("transcript") => match args.get(1) {
            Some(path) if !path.starts_with("--") => {
                cmd_transcript(PathBuf::from(path), &args[2..])
            }
            _ => usage_error("transcript needs a file path."),
        },
        Some("scan") => match args.get(1) {
            Some(path) if !path.starts_with("--") => cmd_scan(PathBuf::from(path), &args[2..]),
            _ => usage_error("scan needs an image path."),
        },
        Some("redact") => match args.get(1) {
            Some(path) if !path.starts_with("--") => cmd_redact(PathBuf::from(path), &args[2..]),
            _ => usage_error("redact needs an image path."),
        },
        Some("models") => match args.get(1).map(String::as_str) {
            None => cmd_models_list(),
            Some("--json") => cmd_models_json(),
            Some("get") => match args.get(2) {
                Some(name) => cmd_models_get(name, args.iter().any(|a| a == "--yes")),
                None => usage_error("models get needs a model name. Run `models` to see them."),
            },
            Some(other) => usage_error(&format!("Unknown models subcommand: {other}")),
        },
        Some("doctor") => match args.get(1).map(String::as_str) {
            None => cmd_doctor(),
            Some("--json") => cmd_doctor_json(),
            Some(other) => usage_error(&format!("Unknown doctor option: {other}")),
        },
        Some("--version" | "-V") => {
            println!("framekeep-core {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("--help" | "-h") | None => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some(other) => usage_error(&format!("Unknown command: {other}")),
    }
}

fn usage_error(msg: &str) -> ExitCode {
    eprintln!("{msg}\n\n{USAGE}");
    ExitCode::from(2)
}

fn cmd_probe(path: PathBuf) -> ExitCode {
    let tools = match ffmpeg::Toolchain::locate() {
        Ok(t) => t,
        Err(e) => return fail(&e),
    };
    match probe::probe(&tools, &path) {
        Ok(report) => emit(&report),
        Err(e) => fail(&e),
    }
}

#[derive(Serialize)]
struct ScanReport {
    language: String,
    #[serde(flatten)]
    frame: scan::FrameScan,
}

fn cmd_scan(path: PathBuf, rest: &[String]) -> ExitCode {
    let patterns = match parse_patterns(rest) {
        Ok(p) => p,
        Err(e) => return usage_error(&e),
    };
    match scan::one(&path, &patterns) {
        Ok((language, frame)) => emit(&ScanReport { language, frame }),
        Err(e) => fail(&e.to_string()),
    }
}

/// `--pattern` on its own, for the two commands that take nothing else.
fn parse_patterns(rest: &[String]) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--pattern" => match rest.get(i + 1) {
                Some(v) => match secrets::check_pattern(v) {
                    Ok(word) if out.len() < secrets::MAX_PATTERNS => out.push(word),
                    Ok(_) => {
                        return Err(format!(
                            "Too many --pattern options; {} is the limit.",
                            secrets::MAX_PATTERNS
                        ))
                    }
                    Err(why) => return Err(format!("--pattern: {why}")),
                },
                None => return Err("--pattern needs a word to hide".into()),
            },
            other => return Err(format!("Unknown option: {other}")),
        }
        i += 2;
    }
    Ok(out)
}

#[derive(Serialize)]
struct RedactReport {
    source: String,
    output: String,
    frame: FrameSize,
    masks_applied: usize,
    /// Present only with `--scan`.
    #[serde(skip_serializing_if = "Option::is_none")]
    from_scan: Option<ScanContribution>,
    verification: Verification,
}

#[derive(Serialize)]
struct FrameSize {
    width: u32,
    height: u32,
}

#[derive(Serialize)]
struct ScanContribution {
    detections: usize,
    /// Detected, and no box to cover it. Redaction is NOT complete on this
    /// frame when this is above zero, and the number exists so that fact cannot
    /// be inferred only by someone who thought to check.
    unmaskable: usize,
}

/// Whether the paint landed, and what a second scan of the finished image saw.
///
/// `boxes_black` is the load-bearing half: every applied mask read back from
/// the file and checked pixel by pixel. Deterministic, needs no OCR engine,
/// and it is the only field a caller may gate on -- learned from the first
/// real approval, where a person had deliberately left two findings visible
/// and a gate on `still_detected` read their decision as a failed paint.
///
/// The OCR half stays, read narrowly, in both directions now. A zero does not
/// mean the frame is clean (the verifier is the same scanner that picked the
/// boxes -- it is blind to what it missed the first time, about one secret in
/// six at 16px). And a non-zero does not mean the paint failed: with a person
/// in the loop it usually means exactly what they chose to leave.
///
/// The field that means safe-to-send does not exist here, because the thing
/// that decides it is a person (S5.4).
#[derive(Serialize)]
struct Verification {
    /// Every applied mask is black in the finished file. The claim the paint
    /// is actually answerable for.
    boxes_black: bool,
    /// False when there was no OCR engine to read the result back with.
    ran: bool,
    still_detected: usize,
}

fn cmd_redact(path: PathBuf, rest: &[String]) -> ExitCode {
    let mut out: Option<PathBuf> = None;
    let mut masks: Vec<redact::Mask> = Vec::new();
    let mut from_scan = false;
    let mut patterns: Vec<String> = Vec::new();
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--out" => {
                match rest.get(i + 1) {
                    Some(v) => out = Some(PathBuf::from(v)),
                    None => return usage_error("--out needs a file path."),
                }
                i += 2;
            }
            "--box" => {
                match rest.get(i + 1).map(|v| redact::Mask::parse(v)) {
                    Some(Ok(m)) => masks.push(m),
                    Some(Err(e)) => return usage_error(&e),
                    None => return usage_error("--box needs x,y,w,h."),
                }
                i += 2;
            }
            "--scan" => {
                from_scan = true;
                i += 1;
            }
            "--pattern" => {
                match rest.get(i + 1) {
                    Some(v) => match secrets::check_pattern(v) {
                        Ok(word) if patterns.len() < secrets::MAX_PATTERNS => patterns.push(word),
                        Ok(_) => {
                            return usage_error(&format!(
                                "Too many --pattern options; {} is the limit.",
                                secrets::MAX_PATTERNS
                            ))
                        }
                        Err(why) => return usage_error(&format!("--pattern: {why}")),
                    },
                    None => return usage_error("--pattern needs a word to hide"),
                }
                i += 2;
            }
            other => return usage_error(&format!("Unknown option: {other}")),
        }
    }

    let Some(dest) = out else {
        return usage_error("redact needs --out: it never writes over the picture it read.");
    };
    let Some(format) = dest
        .extension()
        .and_then(|e| e.to_str())
        .and_then(encode::Format::parse)
    else {
        return usage_error(
            "--out must end in .png or .webp. Lossy formats are not offered: a smeared \
             black edge over text is still text.",
        );
    };

    let tools = match ffmpeg::Toolchain::locate() {
        Ok(t) => t,
        Err(e) => return fail(&e),
    };
    let (width, height) = match redact::dimensions(&tools, &path) {
        Ok(d) => d,
        Err(e) => return fail(&e.to_string()),
    };

    // Scan-derived boxes first, so an explicit --box can never be crowded out
    // by a scan that failed.
    let mut contribution = None;
    if from_scan {
        match scan::one(&path, &patterns) {
            Ok((_, frame)) => {
                let mut unmaskable = 0;
                for d in &frame.detections {
                    if d.boxes.is_empty() {
                        unmaskable += 1;
                    }
                    masks.extend(d.boxes.iter().copied().map(redact::Mask::cover));
                }
                contribution = Some(ScanContribution {
                    detections: frame.detections.len(),
                    unmaskable,
                });
            }
            Err(e) => {
                return fail(&format!(
                    "--scan asked for a scan and could not get one: {e}"
                ))
            }
        }
    }

    if masks.is_empty() {
        return usage_error(
            "Nothing to cover. Pass --box, or --scan, or both. Writing an untouched \
             copy under a name that says `redacted` would be worse than doing nothing.",
        );
    }

    let mut clamped = Vec::with_capacity(masks.len());
    for m in &masks {
        match m.clamp_to(width, height) {
            Some(c) => clamped.push(c),
            None => {
                return fail(&format!(
                    "A box at {},{} is entirely outside this {width}x{height} frame, so it \
                     would cover nothing while the command reported success.\n\
                     Check the coordinates are this frame's own pixels.",
                    m.x, m.y
                ))
            }
        }
    }

    if let Err(e) = redact::apply(&tools, &path, &clamped, &dest, format) {
        return fail(&e.to_string());
    }

    // Painting is not proof. Two read-backs, each answering its own question:
    // are the painted boxes black (the gate), and what does the scanner still
    // see (information -- with a person in the loop, often their own choices).
    let boxes_black = match redact::boxes_are_black(&tools, &dest, &clamped) {
        Ok(b) => b,
        Err(e) => return fail(&e.to_string()),
    };
    let verification = match scan::one(&dest, &patterns) {
        Ok((_, frame)) => Verification {
            boxes_black,
            ran: true,
            still_detected: frame.detections.len(),
        },
        Err(_) => Verification {
            boxes_black,
            ran: false,
            still_detected: 0,
        },
    };

    emit(&RedactReport {
        source: path.display().to_string(),
        output: dest.display().to_string(),
        frame: FrameSize { width, height },
        masks_applied: clamped.len(),
        from_scan: contribution,
        verification,
    })
}

#[derive(Serialize)]
struct FramesReport {
    path: String,
    handle: String,
    output_dir: String,
    selection: SelectionReport,
    dedup: DedupReport,
    frames: Vec<FrameOut>,
    /// `null` when nobody asked to look. See `map::MapRequest::scan` -- the
    /// distinction from an empty summary is the whole point of the field.
    scan: Option<scan::Summary>,
}

#[derive(Serialize)]
struct SelectionReport {
    threshold: f64,
    min_gap_seconds: f64,
    max_gap_seconds: f64,
    frames_selected: usize,
}

#[derive(Serialize)]
struct DedupReport {
    rule: &'static str,
    kept: usize,
    dropped: usize,
    /// Every drop, with its reason. A frame that disappears without a record is
    /// the failure mode this product cannot afford.
    drops: Vec<DropOut>,
}

#[derive(Serialize)]
struct DropOut {
    pts_time: f64,
    reason: &'static str,
}

#[derive(Serialize)]
struct FrameOut {
    pts_time: f64,
    file: String,
}

/// Options shared by `frames` and `map`.
///
/// Parsed in one place on purpose: two copies of this loop is exactly where the
/// two commands would quietly start disagreeing about what `--min-gap` means.
struct Opts {
    params: select::SelectParams,
    out_dir: Option<PathBuf>,
    run_dedup: bool,
    skip_transcript: bool,
    model: Option<PathBuf>,
    format: encode::Format,
    scan: bool,
    /// `--language`, or `None` for whisper's own detection.
    language: Option<String>,
    /// `--pattern`, repeatable. Words a person added in Settings (S5.8); the
    /// scanner takes them as an argument rather than reading anyone's config.
    patterns: Vec<String>,
}

fn parse_opts(rest: &[String]) -> Result<Opts, String> {
    let mut o = Opts {
        params: select::SelectParams::default(),
        out_dir: None,
        run_dedup: true,
        skip_transcript: false,
        model: None,
        format: encode::Format::default(),
        scan: false,
        language: None,
        patterns: Vec::new(),
    };
    let mut i = 0;
    while i < rest.len() {
        let value = rest.get(i + 1);
        match rest[i].as_str() {
            "--threshold" => match value.and_then(|v| v.parse().ok()) {
                Some(v) => o.params.threshold = v,
                None => return Err("--threshold needs a number, e.g. --threshold 0.012".into()),
            },
            "--min-gap" => match value.and_then(|v| v.parse().ok()) {
                Some(v) => o.params.min_gap = v,
                None => return Err("--min-gap needs a number of seconds".into()),
            },
            "--max-gap" => match value.and_then(|v| v.parse().ok()) {
                Some(v) => o.params.max_gap = v,
                None => return Err("--max-gap needs a number of seconds".into()),
            },
            "--out" => match value {
                Some(v) => o.out_dir = Some(PathBuf::from(v)),
                None => return Err("--out needs a folder".into()),
            },
            "--format" => match value.map(String::as_str).and_then(encode::Format::parse) {
                Some(v) => o.format = v,
                None => {
                    return Err(
                        "--format needs `png` or `webp`. Lossy formats are not offered: \
                         artefacts make a model misread the text on screen."
                            .into(),
                    )
                }
            },
            "--model" => match value {
                Some(v) => o.model = Some(PathBuf::from(v)),
                None => return Err("--model needs a path to a .bin model file".into()),
            },
            "--language" => match value {
                Some(v) if v == "auto" => o.language = None,
                Some(v) => o.language = Some(v.clone()),
                None => return Err("--language needs a code like `en`, or `auto`".into()),
            },
            // Checked here rather than swallowed later: a pattern too short to
            // be safe is a typo, and silently ignoring it leaves somebody
            // believing a word is being hidden when nothing is looking for it.
            "--pattern" => match value {
                Some(v) => match secrets::check_pattern(v) {
                    Ok(word) if o.patterns.len() < secrets::MAX_PATTERNS => o.patterns.push(word),
                    Ok(_) => {
                        return Err(format!(
                            "Too many --pattern options; {} is the limit.",
                            secrets::MAX_PATTERNS
                        ))
                    }
                    Err(why) => return Err(format!("--pattern: {why}")),
                },
                None => return Err("--pattern needs a word to hide".into()),
            },
            "--keep-duplicates" => {
                o.run_dedup = false;
                i += 1;
                continue;
            }
            "--scan" => {
                o.scan = true;
                i += 1;
                continue;
            }
            "--skip-transcript" => {
                o.skip_transcript = true;
                i += 1;
                continue;
            }
            other => return Err(format!("Unknown option: {other}")),
        }
        i += 2;
    }
    Ok(o)
}

/// Age old work out of the cache, before doing anything that adds to it.
///
/// This lives here and not only in the tray because the tray may not be
/// installed. Someone running just the MCP server has no queue to expire from,
/// and until this existed their frames and transcripts stayed forever -- which
/// made the seven-day promise true only for the people who installed the app.
///
/// It costs one directory listing, and it runs on the way in so an interrupted
/// run has still swept.
fn sweep_cache() {
    let Some(swept) = cache::sweep_default() else {
        return;
    };
    if swept.removed > 0 {
        eprintln!(
            "Removed {} cached {} older than 7 days.",
            swept.removed,
            if swept.removed == 1 {
                "folder"
            } else {
                "folders"
            }
        );
    }
    if swept.failed > 0 {
        eprintln!(
            "Couldn't remove {} cached {}. Something may still have them open.",
            swept.failed,
            if swept.failed == 1 {
                "folder"
            } else {
                "folders"
            }
        );
    }
}

fn cmd_map(path: PathBuf, rest: &[String]) -> ExitCode {
    sweep_cache();
    let opts = match parse_opts(rest) {
        Ok(o) => o,
        Err(e) => return usage_error(&e),
    };

    let tools = match ffmpeg::Toolchain::locate() {
        Ok(t) => t,
        Err(e) => return fail(&e),
    };

    let handle = match cache::handle_for(&path) {
        Ok(h) => h,
        Err(e) => {
            eprintln!(
                "There's no readable file at {}: {e}\nCheck the path, then try again.",
                path.display()
            );
            return ExitCode::FAILURE;
        }
    };
    let work_dir = match opts.out_dir.clone() {
        Some(d) => d,
        None => match cache::dir_for(&path) {
            Ok(d) => d,
            Err(e) => return fail(&e),
        },
    };

    // Locating whisper is allowed to fail softly: frames are the point of this
    // product, and a missing model should not cost the user the whole map.
    let whisper = if opts.skip_transcript {
        None
    } else {
        match whisper::Whisper::locate(opts.model.as_deref()) {
            Ok(w) => Some(w),
            Err(e) => {
                eprintln!("Continuing without speech -- {e}");
                None
            }
        }
    };

    let req = map::MapRequest {
        video: &path,
        handle,
        work_dir,
        params: opts.params,
        whisper: whisper.as_ref(),
        format: opts.format,
        scan: opts.scan,
        patterns: &opts.patterns,
        language: opts.language.as_deref(),
    };

    match map::build(&tools, req) {
        Ok(m) => emit(&m),
        Err(e) => fail(&e),
    }
}

#[derive(Serialize)]
struct CropReport {
    path: String,
    handle: String,
    output_dir: String,
    region: RegionOut,
    /// What the crop bought. The caller's whole reason for asking is that a
    /// reply cap counts pixel area, so saying how much area went away is saying
    /// how much more they can now fit.
    area_kept_percent: f64,
    frames: Vec<FrameOut>,
}

#[derive(Serialize)]
struct RegionOut {
    x1: u32,
    y1: u32,
    x2: u32,
    y2: u32,
    width: u32,
    height: u32,
}

/// Crops the frames a recording was already broken into.
///
/// Works from the cached frame list rather than re-decoding the video: the
/// pictures are already on disk, and cropping them costs one ffmpeg pass each
/// with no video decode at all.
fn cmd_crop(path: PathBuf, rest: &[String]) -> ExitCode {
    let mut region: Option<encode::Region> = None;
    let mut format = encode::Format::Webp;
    let mut i = 0;
    while i < rest.len() {
        let value = rest.get(i + 1);
        match rest[i].as_str() {
            "--region" => match value.map(|v| encode::Region::parse(v)) {
                Some(Ok(r)) => region = Some(r),
                Some(Err(e)) => return usage_error(&e),
                None => return usage_error("--region needs x1,y1,x2,y2"),
            },
            "--format" => match value.map(String::as_str).and_then(encode::Format::parse) {
                Some(v) => format = v,
                None => return usage_error("--format needs `png` or `webp`"),
            },
            other => return usage_error(&format!("Unknown option: {other}")),
        }
        i += 2;
    }

    let Some(region) = region else {
        return usage_error("crop needs --region x1,y1,x2,y2");
    };

    let tools = match ffmpeg::Toolchain::locate() {
        Ok(t) => t,
        Err(e) => return fail(&e),
    };

    let probed = match probe::probe(&tools, &path) {
        Ok(p) => p,
        Err(e) => return fail(&e),
    };

    // Refuse a rectangle that runs off the frame rather than letting ffmpeg
    // fail with something about filter arguments.
    let (w, h) = (probed.width, probed.height);
    if region.x2 > w || region.y2 > h {
        return fail(&format!(
            "That region runs off the frame. The video is {w}x{h}, \
             and the region asks for up to {},{}.",
            region.x2, region.y2
        ));
    }

    let handle = match cache::handle_for(&path) {
        Ok(h) => h,
        Err(e) => {
            return fail(&format!(
                "There's no readable file at {}: {e}",
                path.display()
            ))
        }
    };
    let work_dir = match cache::dir_for(&path) {
        Ok(d) => d,
        Err(e) => return fail(&e),
    };

    // The source frames, in whatever format they were written in. Falling back
    // to extraction here would hide that `map` was never run, so it does not.
    let params = select::SelectParams::default();
    let source = frame_store::load(&work_dir, &params, encode::Format::Webp)
        .or_else(|| frame_store::load(&work_dir, &params, encode::Format::Png));
    let Some(source) = source else {
        return fail(
            &"No frames have been extracted for this recording yet.\n\
              Run `framekeep-core map <path>` first, then crop."
                .to_string(),
        );
    };

    let dest_dir = work_dir.join(region.dir_name());
    let mut out = Vec::new();
    for frame in &source.frames {
        match encode::crop(&tools, &frame.file, region, &dest_dir, format) {
            Ok(p) => out.push(FrameOut {
                pts_time: frame.pts_time,
                file: p.display().to_string(),
            }),
            Err(e) => return fail(&e),
        }
    }

    let full = (w as f64) * (h as f64);
    let kept = (region.width() as f64) * (region.height() as f64);

    emit(&CropReport {
        path: path.display().to_string(),
        handle,
        output_dir: dest_dir.display().to_string(),
        region: RegionOut {
            x1: region.x1,
            y1: region.y1,
            x2: region.x2,
            y2: region.y2,
            width: region.width(),
            height: region.height(),
        },
        area_kept_percent: if full > 0.0 {
            (kept / full * 1000.0).round() / 10.0
        } else {
            100.0
        },
        frames: out,
    })
}

fn cmd_frames(path: PathBuf, rest: &[String]) -> ExitCode {
    sweep_cache();
    let opts = match parse_opts(rest) {
        Ok(o) => o,
        Err(e) => return usage_error(&e),
    };
    let params = opts.params;
    let out_dir = opts.out_dir;
    let run_dedup = opts.run_dedup;

    let tools = match ffmpeg::Toolchain::locate() {
        Ok(t) => t,
        Err(e) => return fail(&e),
    };

    let handle = match cache::handle_for(&path) {
        Ok(h) => h,
        Err(e) => {
            eprintln!(
                "There's no readable file at {}: {e}\nCheck the path, then try again.",
                path.display()
            );
            return ExitCode::FAILURE;
        }
    };

    // Ask the file whether it is HDR before extracting. An iPhone recording
    // pulled without tone-mapping gives the model a washed-out picture and says
    // nothing about it.
    let hdr = match probe::probe(&tools, &path) {
        Ok(p) => p.is_hdr,
        Err(e) => return fail(&e),
    };

    let out_dir = match out_dir {
        Some(d) => d,
        None => match cache::dir_for(&path) {
            Ok(d) => d,
            Err(e) => return fail(&e),
        },
    };

    let selected = match select::extract(&tools, &path, &out_dir, &params, hdr) {
        Ok(s) => s,
        Err(e) => return fail(&e),
    };
    let frames_selected = selected.len();

    let rule = dedup::ProvablyIdentical::default();
    let mut kept: Vec<select::SelectedFrame> = Vec::new();
    let mut drops: Vec<DropOut> = Vec::new();

    if run_dedup {
        let mut last: Option<dedup::Frame> = None;
        for frame in selected {
            let decoded = match dedup::Frame::load(&frame.file) {
                Ok(d) => d,
                Err(e) => return fail(&e),
            };
            let decision = match &last {
                // Nothing to compare against yet, so nothing can be proven.
                None => Decision::Keep,
                Some(prev) => rule.decide(prev, &decoded),
            };
            match decision {
                Decision::Keep => {
                    last = Some(decoded);
                    kept.push(frame);
                }
                Decision::Drop(reason) => {
                    drops.push(DropOut {
                        pts_time: frame.pts_time,
                        reason,
                    });
                    if let Err(e) = std::fs::remove_file(&frame.file) {
                        eprintln!(
                            "Warning: couldn't delete duplicate frame {}: {e}",
                            frame.file.display()
                        );
                    }
                }
            }
        }
    } else {
        kept = selected;
    }

    // Re-encode last, on survivors only: dedup reads PNG, and encoding frames
    // that are about to be dropped would be work thrown away.
    for frame in &mut kept {
        match encode::convert(&tools, &frame.file, opts.format) {
            Ok(p) => frame.file = p,
            // The PNG is still there and still usable, so this is a warning
            // rather than the end of the run.
            Err(e) => eprintln!("Warning: {e}"),
        }
    }

    let report = FramesReport {
        path: path.display().to_string(),
        handle,
        output_dir: out_dir.display().to_string(),
        selection: SelectionReport {
            threshold: params.threshold,
            min_gap_seconds: params.min_gap,
            max_gap_seconds: params.max_gap,
            frames_selected,
        },
        dedup: DedupReport {
            rule: if run_dedup { rule.name() } else { "disabled" },
            kept: kept.len(),
            dropped: drops.len(),
            drops,
        },
        frames: kept
            .iter()
            .map(|f| FrameOut {
                pts_time: f.pts_time,
                file: f.file.display().to_string(),
            })
            .collect(),
        scan: opts.scan.then(|| {
            scan::many(
                kept.iter().map(|f| (f.pts_time, f.file.as_path())),
                &opts.patterns,
            )
        }),
    };

    emit(&report)
}

fn cmd_transcribe(path: PathBuf, rest: &[String]) -> ExitCode {
    sweep_cache();
    let mut model: Option<PathBuf> = None;
    let mut language: Option<String> = None;
    let mut fresh = false;
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--model" => {
                match rest.get(i + 1) {
                    Some(v) => model = Some(PathBuf::from(v)),
                    None => return usage_error("--model needs a path to a .bin model file"),
                }
                i += 2;
            }
            "--language" => {
                match rest.get(i + 1) {
                    // `auto` is spelled out rather than left to an absent flag,
                    // so a caller can say "detect it" explicitly instead of
                    // hoping the default is what it wants.
                    Some(v) if v == "auto" => language = None,
                    Some(v) => language = Some(v.clone()),
                    None => return usage_error("--language needs a code like `en`, or `auto`."),
                }
                i += 2;
            }
            "--fresh" => {
                fresh = true;
                i += 1;
            }
            other => return usage_error(&format!("Unknown option: {other}")),
        }
    }

    let work_dir = match cache::dir_for(&path) {
        Ok(d) => d,
        Err(e) => return fail(&e),
    };

    // Cheapest possible answer first: someone may already have paid for this.
    // At the default model's ~1x realtime, redoing a two-minute recording costs
    // two minutes for a result already sitting on disk.
    if !fresh {
        if let transcript_store::Status::Ready(stored) = transcript_store::status(&work_dir) {
            return emit(&stored.into_transcript());
        }
    }

    let tools = match ffmpeg::Toolchain::locate() {
        Ok(t) => t,
        Err(e) => return fail(&e),
    };

    // Ask the file before asking whisper. A silent recording is ordinary, and
    // loading a 574 MB model to transcribe nothing would be absurd.
    let probed = match probe::probe(&tools, &path) {
        Ok(p) if !p.has_audio => {
            // Store it too: "this recording has no speech" is a real answer,
            // and rediscovering it on every call is waste with no upside.
            let silent = transcribe::Transcript::silent();
            if let Ok(c) = transcript_store::claim(&work_dir, 0.0) {
                let _ = c.finish(&silent);
            }
            return emit(&silent);
        }
        Ok(p) => p,
        Err(e) => return fail(&e),
    };

    let whisper = match whisper::Whisper::locate(model.as_deref()) {
        Ok(w) => w,
        Err(e) => return fail(&e),
    };

    // Claim before working. Two MCP clients open on the same recording is
    // ordinary, and both running whisper over the same audio is pure waste.
    let claim = match transcript_store::claim(&work_dir, probed.duration_seconds.unwrap_or(0.0)) {
        Ok(c) => c,
        Err(e) => return fail(&e),
    };

    match transcribe::transcribe(&tools, &whisper, &path, &work_dir, language.as_deref()) {
        Ok(t) => {
            if let Err(e) = claim.finish(&t) {
                // The words are in hand; failing to cache them is worth saying
                // out loud but not worth throwing them away over.
                eprintln!("Transcribed, but couldn't save it for next time: {e}");
            }
            emit(&t)
        }
        Err(e) => {
            let _ = claim.fail(&e.to_string());
            fail(&e)
        }
    }
}

/// Reports the state of a recording's transcript without starting any work.
///
/// The whole point is that it is read-only. An MCP adapter polls this while the
/// model is already looking at frames, and a status check that quietly kicked
/// off two minutes of transcription would be a trap rather than an answer.
fn cmd_transcript(path: PathBuf, rest: &[String]) -> ExitCode {
    if let Some(other) = rest.first() {
        return usage_error(&format!("Unknown option: {other}"));
    }
    match cache::dir_for(&path) {
        Ok(dir) => emit(&transcript_store::status(&dir)),
        Err(e) => fail(&e),
    }
}

/// The same catalogue, for a program rather than a person.
///
/// The window needs this to offer a choice, and the alternative -- the app
/// keeping its own list of model names -- is two catalogues that drift the day
/// one gains an entry. One list, in the crate that owns the files.
fn cmd_models_json() -> ExitCode {
    let Some(dir) = whisper::models_dir() else {
        return fail(&"Couldn't work out where models should live.");
    };
    let ram = whisper::total_ram_gb();
    #[derive(Serialize)]
    struct ModelOut {
        name: &'static str,
        file: &'static str,
        size_mib: u32,
        multilingual: bool,
        realtime_factor: f32,
        speed_measured: bool,
        installed: bool,
        path: String,
    }
    #[derive(Serialize)]
    struct Report {
        models_dir: String,
        ram_gb: Option<u32>,
        recommended: Option<&'static str>,
        models: Vec<ModelOut>,
    }
    let models = whisper::MODELS
        .iter()
        .map(|m| {
            let path = models::install_path(&dir, m.file);
            ModelOut {
                name: m.name,
                file: m.file,
                size_mib: m.size_mib,
                multilingual: m.multilingual,
                realtime_factor: m.realtime_factor,
                speed_measured: m.speed_measured,
                installed: path.is_file(),
                path: path.display().to_string(),
            }
        })
        .collect();
    emit(&Report {
        models_dir: dir.display().to_string(),
        ram_gb: ram,
        recommended: ram.map(|r| whisper::recommended_for(r).name),
        models,
    })
}

fn cmd_models_list() -> ExitCode {
    let dir = match whisper::models_dir() {
        Some(d) => d,
        None => return fail(&"Couldn't work out where models should live."),
    };
    let ram = whisper::total_ram_gb();
    let recommended = ram.map(whisper::recommended_for);

    println!("Models live in {}", dir.display());
    if let (Some(ram), Some(r)) = (ram, recommended) {
        println!("This machine has {ram} GB RAM, so: {}", r.name);
    }
    println!();
    println!(
        "{:<22} {:>9} {:>9}  {:<13} STATUS",
        "NAME", "SIZE", "SPEED", "LANGUAGES"
    );
    for m in whisper::MODELS {
        let installed = models::install_path(&dir, m.file).is_file();
        let mark = if installed {
            "installed"
        } else {
            "not installed"
        };
        let star = if recommended.map(|r| r.name) == Some(m.name) {
            "  <- for this machine"
        } else {
            ""
        };
        println!(
            "{:<22} {:>5} MiB {:>8}  {:<13} {mark}{star}",
            m.name,
            m.size_mib,
            m.speed_label(),
            if m.multilingual {
                "many"
            } else {
                "English only"
            }
        );
    }
    println!();
    println!("SPEED is seconds of audio handled per second of waiting, measured on a");
    println!("12-core x64 desktop. At 1x, a two-minute recording takes two minutes to");
    println!("transcribe -- the frames are ready long before the words are.");
    println!("A leading ~ means the figure is interpolated, not timed.");
    println!();
    println!("`framekeep-core models get <name>` shows the exact download size before fetching.");
    ExitCode::SUCCESS
}

/// Two steps on purpose. The default model is over half a gigabyte, and a tool
/// that starts pulling that down because someone ran a command has decided
/// something for them.
fn cmd_models_get(name: &str, confirmed: bool) -> ExitCode {
    let Some(model) = whisper::MODELS.iter().find(|m| m.name == name) else {
        eprintln!("No model called {name}.\nRun `framekeep-core models` to see the list.");
        return ExitCode::from(2);
    };
    let Some(dir) = whisper::models_dir() else {
        return fail(&"Couldn't work out where models should live.");
    };
    let dest = models::install_path(&dir, model.file);

    if dest.is_file() {
        println!("{} is already installed at {}", model.name, dest.display());
        return ExitCode::SUCCESS;
    }

    let info = match models::fetch_info(model.file) {
        Ok(i) => i,
        Err(e) => return fail(&e),
    };

    println!("{}", model.name);
    println!("  size      {:.1} MiB", info.size_mib());
    println!("  file      {}", model.file);
    println!("  goes to   {}", dest.display());
    println!(
        "  languages {}",
        if model.multilingual {
            "many"
        } else {
            "English only"
        }
    );
    match &info.sha256 {
        Some(h) => println!("  checksum  {h}"),
        None => println!("  checksum  not published -- only the size can be verified"),
    }

    if !confirmed {
        println!();
        println!("Nothing downloaded yet. Run the same command with --yes to fetch it.");
        return ExitCode::SUCCESS;
    }

    println!();
    let total = info.size_bytes;
    let mut last_percent = u64::MAX;
    let result = models::download(model.file, &dest, &info, |done, _| {
        let percent = (done * 100).checked_div(total).unwrap_or(0);
        if percent != last_percent {
            last_percent = percent;
            // stderr, so stdout stays machine-readable.
            eprint!(
                "\r  {percent:>3}%  {:.0}/{:.0} MiB",
                done as f64 / 1_048_576.0,
                total as f64 / 1_048_576.0
            );
            let _ = std::io::Write::flush(&mut std::io::stderr());
        }
    });
    eprintln!();

    match result {
        Ok(()) => {
            println!("Installed {}", dest.display());
            ExitCode::SUCCESS
        }
        Err(e) => fail(&e),
    }
}

/// Exists because "it doesn't work" is the least useful bug report we can get.
fn cmd_doctor() -> ExitCode {
    println!("framekeep-core {}", env!("CARGO_PKG_VERSION"));
    println!(
        "built for   {}",
        ffmpeg::Arch::current()
            .map(|a| a.to_string())
            .unwrap_or_else(|| std::env::consts::ARCH.to_string())
    );

    match ffmpeg::Toolchain::locate() {
        Ok(tools) => {
            println!("ffmpeg      {}", tools.ffmpeg.display());
            println!("ffprobe     {}", tools.ffprobe.display());
            if let Some(r) = cache::root() {
                println!("cache       {}", r.display());
                // Say what is in there and when it goes. A retention promise
                // nobody can check is a claim, not a promise.
                let folders = std::fs::read_dir(&r)
                    .map(|d| d.flatten().filter(|e| e.path().is_dir()).count())
                    .unwrap_or(0);
                println!(
                    "            {folders} folder{}, removed after 7 days without use",
                    if folders == 1 { "" } else { "s" }
                );
            }
            match whisper::Whisper::locate(None) {
                Ok(w) => {
                    println!("whisper     {}", w.cli.display());
                    println!("model       {}", w.model.display());
                }
                Err(e) => {
                    println!();
                    println!("Speech is unavailable, frames still work:");
                    for line in e.to_string().lines() {
                        println!("  {line}");
                    }
                    if let Some(ram) = whisper::total_ram_gb() {
                        let m = whisper::recommended_for(ram);
                        println!(
                            "  This machine has {ram} GB RAM -> {} ({} MiB).",
                            m.name, m.size_mib
                        );
                    }
                }
            }
            if tools.from_path {
                println!();
                println!("WARNING: these came from PATH, not from the bundle.");
                println!("Version and licence build are not under Framekeep's control here.");
                println!("Fine for development; not what ships.");
            }
            ExitCode::SUCCESS
        }
        Err(e) => fail(&e),
    }
}

/// `doctor --json` -- the same check, for something that is not a person.
///
/// Exists because the Settings screen needs these answers and had no way to ask
/// for them. Advanced shows which ffmpeg will run and where it came from; About
/// shows the versions; Privacy shows where the cache lives and how long it
/// stays. Parsing the human `doctor` output for that would make its wording
/// load-bearing, so this is the machine-readable half of one command rather
/// than a second source of truth.
///
/// **A missing toolchain is still a report, and reporting it is success.** The
/// text form fails outright, which is right for a person reading a terminal --
/// and it stays the form to use as a check. This one answers a window asking
/// "what is installed?", where "nothing, and here is why" is the answer rather
/// than an error, and it carries that inside `ffmpeg_error`.
///
/// Said plainly because the alternative was tried first: a failing exit code
/// here reaches the app through `CoreBinary::run`, which turns any non-zero
/// status into an `Err` built from stderr -- and stderr is empty, because the
/// report went to stdout. The screen would have shown *"framekeep-core doctor
/// failed without saying why"* in exactly the situation the report exists to
/// explain.
fn cmd_doctor_json() -> ExitCode {
    #[derive(Serialize)]
    struct FfmpegOut {
        path: String,
        ffprobe: String,
        /// The whole build token, for bug reports.
        version: Option<String>,
        /// Just the numbers, for a settings line.
        version_short: Option<String>,
        /// True when these came off PATH: not the build we tested or licensed.
        from_path: bool,
    }
    #[derive(Serialize)]
    struct WhisperOut {
        available: bool,
        cli: Option<String>,
        model: Option<String>,
        /// The reason, in the words already written for a person to read.
        unavailable: Option<String>,
    }
    #[derive(Serialize)]
    struct CacheOut {
        root: Option<String>,
        folders: usize,
        keep_days: u64,
    }
    /// The frame-selection numbers, reported rather than restated.
    ///
    /// The Advanced screen says what these are, and every one of them was
    /// argued for from a measurement (`select.rs`). A copy of them written
    /// into the window would be a second place to change and the first place
    /// to go stale -- the same reason the ffmpeg version is asked of ffmpeg.
    #[derive(Serialize)]
    struct SelectionOut {
        scene_threshold: f64,
        min_gap_seconds: f64,
        max_gap_seconds: f64,
    }
    #[derive(Serialize)]
    struct Report {
        core_version: &'static str,
        arch: String,
        ffmpeg: Option<FfmpegOut>,
        ffmpeg_error: Option<String>,
        whisper: WhisperOut,
        cache: CacheOut,
        selection: SelectionOut,
        models_dir: Option<String>,
        ram_gb: Option<u32>,
    }

    let tools = ffmpeg::Toolchain::locate();
    let ffmpeg_error = tools.as_ref().err().map(|e| e.to_string());
    let ffmpeg = tools.as_ref().ok().map(|t| {
        let version = t.version();
        FfmpegOut {
            path: t.ffmpeg.display().to_string(),
            ffprobe: t.ffprobe.display().to_string(),
            version_short: version.as_ref().map(|v| v.short.clone()),
            version: version.map(|v| v.full),
            from_path: t.from_path,
        }
    });

    let whisper = match whisper::Whisper::locate(None) {
        Ok(w) => WhisperOut {
            available: true,
            cli: Some(w.cli.display().to_string()),
            model: Some(w.model.display().to_string()),
            unavailable: None,
        },
        Err(e) => WhisperOut {
            available: false,
            cli: None,
            model: None,
            unavailable: Some(e.to_string()),
        },
    };

    let root = cache::root();
    let report = Report {
        core_version: env!("CARGO_PKG_VERSION"),
        arch: ffmpeg::Arch::current()
            .map(|a| a.to_string())
            .unwrap_or_else(|| std::env::consts::ARCH.to_string()),
        ffmpeg,
        ffmpeg_error,
        whisper,
        cache: CacheOut {
            folders: root
                .as_ref()
                .and_then(|r| std::fs::read_dir(r).ok())
                .map(|d| d.flatten().filter(|e| e.path().is_dir()).count())
                .unwrap_or(0),
            root: root.map(|r| r.display().to_string()),
            keep_days: cache::KEEP_FOR.as_secs() / (24 * 60 * 60),
        },
        selection: {
            let p = select::SelectParams::default();
            SelectionOut {
                scene_threshold: p.threshold,
                min_gap_seconds: p.min_gap,
                max_gap_seconds: p.max_gap,
            }
        },
        models_dir: whisper::models_dir().map(|d| d.display().to_string()),
        ram_gb: whisper::total_ram_gb(),
    };

    emit(&report)
}

fn emit<T: Serialize>(value: &T) -> ExitCode {
    match serde_json::to_string_pretty(value) {
        Ok(json) => {
            println!("{json}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("Could not serialise the result: {e}");
            ExitCode::FAILURE
        }
    }
}

fn fail(e: &dyn std::fmt::Display) -> ExitCode {
    eprintln!("{e}");
    ExitCode::FAILURE
}
