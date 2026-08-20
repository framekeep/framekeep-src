//! Turning the audio of a screen recording into timestamped text.
//!
//! The transcript is a **guide, not the content**. What the user said while
//! recording tells the model where to look; the frames are what it looks at.
//! That is why every segment carries its start and end time -- so the model can
//! ask for the frames around a sentence rather than reading the words instead
//! of the picture.
//!
//! A recording with no audio is normal, not an error. Plenty of screen captures
//! have no microphone at all.

use serde::Serialize;
use std::ffi::OsString;
use std::fmt;
use std::path::Path;

use crate::ffmpeg::Toolchain;
use crate::whisper::Whisper;

#[derive(Debug)]
pub enum TranscribeError {
    AudioExtractionFailed { stderr: String },
    WhisperLaunchFailed(std::io::Error),
    WhisperFailed { stderr: String },
    UnreadableOutput { detail: String },
    Io(std::io::Error),
}

impl fmt::Display for TranscribeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TranscribeError::AudioExtractionFailed { stderr } => write!(
                f,
                "Couldn't pull the audio out of that video.\n\
                 ffmpeg said: {}",
                last_line(stderr)
            ),
            TranscribeError::WhisperLaunchFailed(e) => write!(
                f,
                "Couldn't start whisper: {e}\nThe bundled tools may be missing or blocked by security software."
            ),
            TranscribeError::WhisperFailed { stderr } => write!(
                f,
                "whisper couldn't transcribe that audio.\nwhisper said: {}",
                last_line(stderr)
            ),
            TranscribeError::UnreadableOutput { detail } => write!(
                f,
                "whisper produced output Framekeep couldn't read: {detail}\n\
                 This is a bug in Framekeep -- please report it."
            ),
            TranscribeError::Io(e) => write!(f, "File error while transcribing: {e}"),
        }
    }
}

fn last_line(s: &str) -> &str {
    s.lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .unwrap_or("(no output)")
}

/// How many threads to hand whisper.
///
/// All of them. Transcription is the slowest thing this tool does, and it runs
/// while the user is waiting for a paste to finish -- there is nothing else
/// competing for the machine at that moment.
pub fn worker_threads() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}

#[derive(Debug, Serialize)]
pub struct Segment {
    pub start_seconds: f64,
    pub end_seconds: f64,
    pub text: String,
}

#[derive(Debug, Serialize)]
pub struct Transcript {
    /// False when the video carries no audio track at all. Not a failure --
    /// a silent screen capture is a perfectly ordinary thing to hand us.
    pub has_audio: bool,
    pub model: Option<String>,
    pub segments: Vec<Segment>,
}

impl Transcript {
    pub fn silent() -> Self {
        Transcript {
            has_audio: false,
            model: None,
            segments: Vec::new(),
        }
    }
}

/// A file that deletes itself when it goes out of scope.
///
/// Transcription writes a 16 kHz copy of the user's voice to disk. The delete
/// used to sit at the end of the happy path, which meant **every** failure --
/// ffmpeg refusing the file, whisper running out of memory, a panic -- left
/// that copy behind. A promise about someone's recorded audio cannot depend on
/// nothing going wrong, so the delete moved into a destructor where the failure
/// paths cannot miss it.
struct Ephemeral(std::path::PathBuf);

impl Drop for Ephemeral {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Deletes an audio copy stranded by a process that was killed mid-transcription.
///
/// A destructor cannot run when the process is terminated outright, so that one
/// case needs sweeping from outside. Called when a transcript slot is claimed:
/// the only writer is about to overwrite this file anyway.
pub fn sweep_stranded_audio(work_dir: &Path) {
    let _ = std::fs::remove_file(work_dir.join("audio-16k.wav"));
}

/// Extracts 16 kHz mono PCM -- the only shape whisper.cpp accepts -- then runs
/// whisper over it.
///
/// `work_dir` holds the intermediate wav. It is removed on every exit path,
/// including the failing ones: a copy of the user's audio should not outlive
/// the job that needed it.
pub fn transcribe(
    tools: &Toolchain,
    whisper: &Whisper,
    video: &Path,
    work_dir: &Path,
    language: Option<&str>,
) -> Result<Transcript, TranscribeError> {
    std::fs::create_dir_all(work_dir).map_err(TranscribeError::Io)?;
    let wav = work_dir.join("audio-16k.wav");
    let _audio_guard = Ephemeral(wav.clone());

    let args: Vec<OsString> = vec![
        "-hide_banner".into(),
        "-nostdin".into(),
        "-y".into(),
        "-i".into(),
        video.as_os_str().to_owned(),
        "-vn".into(),
        "-ar".into(),
        "16000".into(),
        "-ac".into(),
        "1".into(),
        "-c:a".into(),
        "pcm_s16le".into(),
        wav.clone().into_os_string(),
    ];

    let out = tools.run_ffmpeg(&args).map_err(TranscribeError::Io)?;
    if !out.status.success() {
        return Err(TranscribeError::AudioExtractionFailed {
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }

    // whisper's own scratch output, on the same terms as the audio.
    //
    // Named `whisper-out`, not `transcript`: the store keeps its result at
    // `transcript.json` in this same folder. Two different files under one name
    // would have collided the moment a run failed before cleaning up, and the
    // store would have read whisper's raw output as a corrupt transcript.
    let json_stem = work_dir.join("whisper-out");
    let _json_guard = Ephemeral(json_stem.with_extension("json"));
    let w_args: Vec<OsString> = vec![
        "-m".into(),
        whisper.model.clone().into_os_string(),
        "-f".into(),
        wav.clone().into_os_string(),
        // whisper-cli defaults to 4 threads regardless of the machine. Measured
        // on a 12-core box: 155s at 4 threads, 108s at 12 -- a 44% saving that
        // was being left on the floor for the sake of one argument.
        "-t".into(),
        worker_threads().to_string().into(),
        "-oj".into(),
        "-of".into(),
        json_stem.clone().into_os_string(),
        "--no-prints".into(),
    ];

    // Absent means whisper detects it, which is right far more often than a
    // guess would be. Naming a language is for the case detection gets wrong
    // -- a recording that is mostly silence with a few English words in it,
    // where whisper has picked something else and transcribed nonsense.
    let mut w_args = w_args;
    if let Some(code) = language {
        w_args.push("-l".into());
        w_args.push(code.into());
    }

    let out = whisper
        .run(&w_args)
        .map_err(TranscribeError::WhisperLaunchFailed)?;
    if !out.status.success() {
        return Err(TranscribeError::WhisperFailed {
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }

    let json_path = json_stem.with_extension("json");
    let raw = std::fs::read(&json_path).map_err(TranscribeError::Io)?;
    let parsed: serde_json::Value =
        serde_json::from_slice(&raw).map_err(|e| TranscribeError::UnreadableOutput {
            detail: e.to_string(),
        })?;

    let segments = parsed
        .get("transcription")
        .and_then(|t| t.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let text = item.get("text")?.as_str()?.trim().to_string();
                    if text.is_empty() {
                        return None;
                    }
                    let offsets = item.get("offsets")?;
                    Some(Segment {
                        // whisper.cpp reports offsets in milliseconds.
                        start_seconds: offsets.get("from")?.as_f64()? / 1000.0,
                        end_seconds: offsets.get("to")?.as_f64()? / 1000.0,
                        text,
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    // Both scratch files are removed by their guards on the way out of this
    // function, whichever way it exits.
    Ok(Transcript {
        has_audio: true,
        model: whisper
            .model
            .file_name()
            .map(|n| n.to_string_lossy().into_owned()),
        segments,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_silent_recording_is_not_a_failure() {
        let t = Transcript::silent();
        assert!(!t.has_audio);
        assert!(t.segments.is_empty());
        assert!(
            t.model.is_none(),
            "no model was used, so none should be claimed"
        );
    }

    #[test]
    fn transcript_serialises_with_timestamps() {
        // The timestamps are the point: they are what lets a model ask for the
        // frames around a sentence instead of reading words in place of the
        // picture.
        let t = Transcript {
            has_audio: true,
            model: Some("ggml-tiny.en.bin".into()),
            segments: vec![Segment {
                start_seconds: 1.5,
                end_seconds: 3.25,
                text: "here is the bug".into(),
            }],
        };
        let json = serde_json::to_string(&t).unwrap();
        assert!(json.contains("\"start_seconds\":1.5"));
        assert!(json.contains("\"end_seconds\":3.25"));
    }
}
