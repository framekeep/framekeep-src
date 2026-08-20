//! `framekeep-core probe <path>` -- what is this file, and can we work with it?
//!
//! Deliberately says nothing about the *content* of the video. This is the eye
//! reporting what it can see, not a judgement about what the video means.

use serde::Serialize;
use std::fmt;
use std::path::Path;

use crate::ffmpeg::Toolchain;

#[derive(Debug)]
pub enum ProbeError {
    NotFound(String),
    NotAFile(String),
    FfprobeFailed {
        path: String,
        stderr: String,
    },
    LaunchFailed {
        path: String,
        source: std::io::Error,
    },
    Unreadable {
        path: String,
        detail: String,
    },
    NoVideoStream(String),
}

impl fmt::Display for ProbeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProbeError::NotFound(p) => {
                write!(
                    f,
                    "There's no file at {p}.\nCheck the path, then try again."
                )
            }
            ProbeError::NotAFile(p) => {
                write!(
                    f,
                    "{p} is a folder, not a video file.\nPick the file itself."
                )
            }
            ProbeError::LaunchFailed { path, source } => write!(
                f,
                "Couldn't start ffprobe ({path}): {source}\n\
                 The bundled tools may be missing or blocked by security software."
            ),
            ProbeError::FfprobeFailed { path, stderr } => {
                // ffprobe already distinguishes these cases; passing them through as
                // one generic message throws away the only useful thing it said.
                let hint = if stderr.contains("moov atom not found") {
                    // The index lives at the end of an MP4, so a recording that is
                    // still running -- or was interrupted -- has no moov atom yet.
                    "That video has no index yet, which usually means the recording is still running or was interrupted.\nStop the recording and try again."
                } else if stderr.contains("Invalid data found") {
                    "That file isn't a video.\nPick a screen recording -- mp4, mov, mkv or webm."
                } else if stderr.contains("Permission denied") {
                    "Can't read that file.\nClose whatever is using it, or pick it again to grant access."
                } else {
                    "ffmpeg couldn't read that file. It may be corrupted, or still being recorded.\nIf the recording has finished, the file may be damaged."
                };
                write!(
                    f,
                    "{hint}\nFile: {path}\nffprobe said: {}",
                    first_line(stderr)
                )
            }
            ProbeError::Unreadable { path, detail } => write!(
                f,
                "ffprobe returned something unexpected for {path}: {detail}\n\
                 This is a bug in Framekeep -- please report it with the file's format."
            ),
            ProbeError::NoVideoStream(p) => write!(
                f,
                "{p} has no video track -- Framekeep has nothing to show your AI.\n\
                 If this is an audio file, use a transcription tool instead."
            ),
        }
    }
}

fn first_line(s: &str) -> &str {
    s.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("(no output)")
}

/// What `probe` reports. Every field is measured, none is inferred.
#[derive(Debug, Serialize)]
pub struct Probe {
    pub path: String,
    pub duration_seconds: Option<f64>,
    pub width: u32,
    pub height: u32,
    pub video_codec: String,
    /// Average frame rate, from `avg_frame_rate`. `None` when ffprobe reports
    /// `0/0`, which happens with some variable-frame-rate screen recordings.
    pub fps: Option<f64>,
    pub has_audio: bool,
    pub audio_codec: Option<String>,
    pub container: Option<String>,
    pub size_bytes: Option<u64>,
    /// Transfer characteristics as ffprobe reports them, e.g. `bt709`,
    /// `smpte2084` (PQ), `arib-std-b67` (HLG).
    pub color_transfer: Option<String>,
    pub color_primaries: Option<String>,
    /// True for PQ or HLG content -- iPhones on iOS 18+ record this by default.
    ///
    /// Frames pulled from HDR without tone-mapping come out washed out and
    /// flat: measured on PQ content, saturation drops 51% and contrast 70%.
    /// A model reading that picture is reading a worse picture than the user
    /// saw, and nothing about the output says so.
    pub is_hdr: bool,
}

/// Transfer curves that mean "this is HDR". Anything else -- including
/// `unknown`, which is most SDR screen recordings -- is treated as SDR.
fn transfer_is_hdr(transfer: Option<&str>) -> bool {
    matches!(
        transfer,
        Some("smpte2084" | "arib-std-b67" | "smpte428" | "bt2020-10" | "bt2020-12")
    )
}

pub fn probe(tools: &Toolchain, path: &Path) -> Result<Probe, ProbeError> {
    let display = path.display().to_string();

    if !path.exists() {
        return Err(ProbeError::NotFound(display));
    }
    if !path.is_file() {
        return Err(ProbeError::NotAFile(display));
    }

    // Array arguments, straight through to the OS. The path is one element, so
    // spaces and non-ASCII characters never meet a shell.
    let args: Vec<&std::ffi::OsStr> = vec![
        "-v".as_ref(),
        "error".as_ref(),
        "-print_format".as_ref(),
        "json".as_ref(),
        "-show_format".as_ref(),
        "-show_streams".as_ref(),
        path.as_os_str(),
    ];

    let out = tools
        .run_ffprobe(&args)
        .map_err(|source| ProbeError::LaunchFailed {
            path: tools.ffprobe.display().to_string(),
            source,
        })?;

    if !out.status.success() {
        return Err(ProbeError::FfprobeFailed {
            path: display,
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }

    let json: serde_json::Value =
        serde_json::from_slice(&out.stdout).map_err(|e| ProbeError::Unreadable {
            path: display.clone(),
            detail: e.to_string(),
        })?;

    let streams = json
        .get("streams")
        .and_then(|s| s.as_array())
        .ok_or_else(|| ProbeError::Unreadable {
            path: display.clone(),
            detail: "no streams array".into(),
        })?;

    let video = streams
        .iter()
        .find(|s| s.get("codec_type").and_then(|t| t.as_str()) == Some("video"))
        .ok_or_else(|| ProbeError::NoVideoStream(display.clone()))?;

    let audio = streams
        .iter()
        .find(|s| s.get("codec_type").and_then(|t| t.as_str()) == Some("audio"));

    let format = json.get("format");

    // Duration lives on the container for most files and on the stream for some.
    let duration_seconds = format
        .and_then(|f| f.get("duration"))
        .and_then(|d| d.as_str())
        .and_then(|d| d.parse::<f64>().ok())
        .or_else(|| {
            video
                .get("duration")
                .and_then(|d| d.as_str())
                .and_then(|d| d.parse::<f64>().ok())
        })
        .filter(|d| d.is_finite() && *d > 0.0);

    let color_transfer = video
        .get("color_transfer")
        .and_then(|v| v.as_str())
        .filter(|s| *s != "unknown")
        .map(str::to_string);

    Ok(Probe {
        is_hdr: transfer_is_hdr(color_transfer.as_deref()),
        color_primaries: video
            .get("color_primaries")
            .and_then(|v| v.as_str())
            .filter(|s| *s != "unknown")
            .map(str::to_string),
        color_transfer,
        path: display,
        duration_seconds,
        width: video.get("width").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
        height: video.get("height").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
        video_codec: video
            .get("codec_name")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string(),
        fps: video
            .get("avg_frame_rate")
            .and_then(|v| v.as_str())
            .and_then(parse_rational),
        has_audio: audio.is_some(),
        audio_codec: audio
            .and_then(|a| a.get("codec_name"))
            .and_then(|v| v.as_str())
            .map(str::to_string),
        container: format
            .and_then(|f| f.get("format_name"))
            .and_then(|v| v.as_str())
            .map(str::to_string),
        size_bytes: format
            .and_then(|f| f.get("size"))
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok()),
    })
}

/// ffprobe reports frame rates as `30000/1001`. `0/0` means it could not work
/// one out -- common with variable-frame-rate screen recordings, and a real
/// answer rather than an error.
fn parse_rational(s: &str) -> Option<f64> {
    let (num, den) = s.split_once('/')?;
    let num: f64 = num.trim().parse().ok()?;
    let den: f64 = den.trim().parse().ok()?;
    (den != 0.0 && num != 0.0).then(|| num / den)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ntsc_frame_rates() {
        let fps = parse_rational("30000/1001").unwrap();
        assert!((fps - 29.97).abs() < 0.01);
        assert_eq!(parse_rational("60/1"), Some(60.0));
    }

    #[test]
    fn hdr_transfer_curves_are_recognised() {
        // iPhone iOS 18+ records PQ by default; HLG shows up from broadcast
        // sources and some Android phones.
        assert!(transfer_is_hdr(Some("smpte2084")), "PQ is HDR");
        assert!(transfer_is_hdr(Some("arib-std-b67")), "HLG is HDR");
    }

    #[test]
    fn ordinary_screen_recordings_are_not_treated_as_hdr() {
        // Most screen captures report bt709 or nothing at all. Tone-mapping
        // those would damage a perfectly good picture.
        assert!(!transfer_is_hdr(Some("bt709")));
        assert!(!transfer_is_hdr(None));
        assert!(!transfer_is_hdr(Some("unknown")));
    }

    #[test]
    fn unknown_frame_rate_is_none_not_an_error() {
        // Variable-frame-rate screen recordings report this; it is not a failure.
        assert_eq!(parse_rational("0/0"), None);
        assert_eq!(parse_rational("garbage"), None);
    }

    #[test]
    fn error_messages_say_what_to_do_next() {
        // AGENTS.md: every error says where it broke and what to do next.
        let msg = ProbeError::NotFound("C:\\x.mp4".into()).to_string();
        assert!(msg.contains("try again"), "no next step in: {msg}");

        let msg = ProbeError::NoVideoStream("C:\\x.m4a".into()).to_string();
        assert!(msg.contains("instead"), "no next step in: {msg}");
    }

    /// A recording still in progress is a normal thing for this product to meet,
    /// and it must not be reported as a damaged file.
    #[test]
    fn still_recording_is_told_apart_from_not_a_video() {
        let still_recording = ProbeError::FfprobeFailed {
            path: "C:\\rec.mp4".into(),
            stderr: "[mov,mp4 @ 0x1] moov atom not found".into(),
        }
        .to_string();
        assert!(
            still_recording.contains("still running"),
            "got: {still_recording}"
        );

        let not_a_video = ProbeError::FfprobeFailed {
            path: "C:\\notes.txt".into(),
            stderr: "notes.txt: Invalid data found when processing input".into(),
        }
        .to_string();
        assert!(not_a_video.contains("isn't a video"), "got: {not_a_video}");
        assert!(
            !not_a_video.contains("still running"),
            "a text file should not be described as a recording in progress: {not_a_video}"
        );
    }
}
