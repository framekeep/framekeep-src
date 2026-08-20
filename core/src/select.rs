//! Choosing which frames a screen recording is actually made of.
//!
//! A screen recording stands still most of the time, so a fixed frame rate is
//! both wasteful and lossy. Three gates decide, and they answer different
//! questions:
//!
//!   * **scene threshold** -- did the picture change enough to be worth seeing?
//!   * **min-gap** -- has enough time passed that this is a new moment rather
//!     than the same one mid-animation?
//!   * **max-gap** -- has nothing changed for so long that we owe the model a
//!     frame anyway?
//!
//! Without max-gap a long unchanging stretch yields nothing at all, and the
//! model is told a video exists with no evidence of what was on screen.
//!
//! Timestamps come from `showinfo` placed *after* `select`, parsed out of
//! stderr. That is the cheap way to keep the original presentation times
//! through variable frame rate, through dedup, and through file renaming.

use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::ffmpeg::Toolchain;

/// The sourced 0.1-0.2 range describes *slide transitions* -- whole-screen
/// swaps. Measured on actual screen work it is an order of magnitude too high:
/// switching to a different screen in an IDE scores about **0.015**, and
/// continuous scrolling scores about **0.078**. See
/// `docs/experiments/scene-threshold-screen-recording.md`.
///
/// The floor therefore has to reach below where real UI changes live, or the
/// scene gate can never fire at all and max-gap silently does all the work.
pub const THRESHOLD_MIN: f64 = 0.002;
pub const THRESHOLD_MAX: f64 = 0.60;

/// HDR to BT.709, so a PNG shows what the user saw.
///
/// iPhones on iOS 18+ record HDR by default, and a frame pulled straight out of
/// PQ without this looks washed out and flat. Measured on PQ content: mean
/// saturation 115 -> 236 and contrast 23 -> 75. The model would otherwise be
/// reading a degraded picture with nothing in the output to say so.
///
/// `hable` because it keeps highlight detail rather than clipping it -- screen
/// content is full of bright flat regions where clipping is exactly the failure
/// that loses text. `desat=0` because desaturation is meant to hide artefacts
/// in film grain, and it costs colour accuracy we need for UI elements.
///
/// **Costs 6.4x**: extracting frames from a 60-second HDR clip goes from 1.67s
/// to 10.72s. Worth it -- an unreadable frame is worth nothing at any speed --
/// but it means "frames are ready in 2.3 seconds" is an SDR figure. An iPhone
/// recording pays roughly ten times that.
///
/// Only ever applied to HDR sources, and that guard is load-bearing rather than
/// tidy: run this chain against SDR input and ffmpeg fails outright with
/// `no path between colorspaces`.
const TONE_MAP: &str = "zscale=t=linear:npl=100,format=gbrpf32le,\
                        zscale=p=bt709,tonemap=tonemap=hable:desat=0,\
                        zscale=t=bt709:m=bt709:r=tv,format=yuv420p";

#[derive(Debug, Clone, Copy)]
pub struct SelectParams {
    /// Scene-change score above which a frame is worth keeping, 0.0-1.0.
    pub threshold: f64,
    /// Seconds that must pass after a kept frame before another may be kept.
    pub min_gap: f64,
    /// Seconds after which a frame is kept even with no scene change.
    pub max_gap: f64,
}

impl Default for SelectParams {
    fn default() -> Self {
        // Measured, not inherited. At the old 0.12 the scene gate never fired
        // on real screen content at all: every frame came from max-gap, which
        // is fixed-interval sampling wearing a costume.
        //
        // 0.012 sits just under a full screen change (~0.015) and well under
        // scrolling (~0.078). min-gap then has to carry the scroll case,
        // because scrolling scores *higher* than the content changes we want --
        // 5s is what holds 30s of continuous scrolling to 6 frames.
        //
        // Cost, stated plainly: two meaningful screens less than 5s apart
        // collapse into one. That is the price of using a pixel-difference
        // signal on screen content, and it is the same wall the dedup
        // experiment hit. Calibrated on a synthetic corpus -- recheck against
        // real recordings.
        SelectParams {
            threshold: 0.012,
            min_gap: 5.0,
            max_gap: 15.0,
        }
    }
}

impl SelectParams {
    pub fn validate(&self) -> Result<(), String> {
        if !(THRESHOLD_MIN..=THRESHOLD_MAX).contains(&self.threshold) {
            return Err(format!(
                "Scene threshold {:.4} is outside the useful range {THRESHOLD_MIN}-{THRESHOLD_MAX}.\n\
                 On real screen recordings a full screen change scores about 0.015 and\n\
                 continuous scrolling about 0.078, so useful values are far below the\n\
                 0.1-0.2 figure quoted for slide transitions.",
                self.threshold
            ));
        }
        if self.min_gap < 0.0 {
            return Err("Minimum gap can't be negative.".into());
        }
        if self.max_gap <= self.min_gap {
            return Err(format!(
                "Maximum gap ({:.1}s) must be larger than the minimum gap ({:.1}s),\n\
                 otherwise every frame is forced and the scene threshold does nothing.",
                self.max_gap, self.min_gap
            ));
        }
        Ok(())
    }

    /// The `select` expression, built once so the reasoning lives in one place.
    ///
    /// The first frame is always taken: `prev_selected_t` is NaN until
    /// something has been selected, and a video whose opening frame is missing
    /// is missing the one frame that says what the user was looking at.
    pub fn filter_expr(&self) -> String {
        format!(
            "select='if(isnan(prev_selected_t),1,\
             max(gt(scene,{th})*gte(t-prev_selected_t,{min}),\
             gte(t-prev_selected_t,{max})))',showinfo",
            th = self.threshold,
            min = self.min_gap,
            max = self.max_gap,
        )
    }

    /// The full filter chain, with HDR handled first when the source needs it.
    ///
    /// Tone-mapping runs *before* selection so scene scores are computed on the
    /// same picture that gets written -- scoring PQ values and then writing
    /// BT.709 ones would be measuring a different video than we save.
    pub fn filter_chain(&self, hdr: bool) -> String {
        if hdr {
            format!("{TONE_MAP},{}", self.filter_expr())
        } else {
            self.filter_expr()
        }
    }
}

/// One frame that survived selection. `pts_time` is the timestamp in the
/// *source* video -- the only timestamp worth reporting to a model, and the
/// reason `showinfo` is placed after `select` rather than before it.
#[derive(Debug, Clone)]
pub struct SelectedFrame {
    pub pts_time: f64,
    pub file: PathBuf,
}

#[derive(Debug)]
pub enum SelectError {
    BadParams(String),
    LaunchFailed(std::io::Error),
    FfmpegFailed { stderr: String },
    NoFramesWritten { stderr: String },
    CountMismatch { timestamps: usize, files: usize },
}

impl fmt::Display for SelectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SelectError::BadParams(m) => write!(f, "{m}"),
            SelectError::LaunchFailed(e) => write!(
                f,
                "Couldn't start ffmpeg: {e}\nThe bundled tools may be missing or blocked by security software."
            ),
            SelectError::FfmpegFailed { stderr } => write!(
                f,
                "ffmpeg couldn't extract frames from that video.\nffmpeg said: {}",
                last_meaningful_line(stderr)
            ),
            SelectError::NoFramesWritten { stderr } => write!(
                f,
                "ffmpeg ran but wrote no frames.\n\
                 If the video is very short, lower the minimum gap. If it never changes,\n\
                 lower the maximum gap so a frame is forced.\nffmpeg said: {}",
                last_meaningful_line(stderr)
            ),
            // Trust neither side when they disagree: a silently mismatched pair
            // would attach wrong timestamps to real frames, which is worse than
            // failing.
            SelectError::CountMismatch { timestamps, files } => write!(
                f,
                "ffmpeg reported {timestamps} frames but wrote {files} files.\n\
                 Refusing to guess which timestamp belongs to which frame.\n\
                 This is a bug in Framekeep -- please report it with the video's format."
            ),
        }
    }
}

fn last_meaningful_line(stderr: &str) -> &str {
    stderr
        .lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty() && !l.starts_with("frame=") && !l.contains("Parsed_showinfo"))
        .unwrap_or("(no output)")
}

/// Runs one ffmpeg pass that both selects and writes the frames.
///
/// PNG, not JPEG: this product exists so a model can read what is on screen,
/// and JPEG artefacts are worst exactly on small UI text.
pub fn extract(
    tools: &Toolchain,
    video: &Path,
    out_dir: &Path,
    params: &SelectParams,
    hdr: bool,
) -> Result<Vec<SelectedFrame>, SelectError> {
    params.validate().map_err(SelectError::BadParams)?;
    std::fs::create_dir_all(out_dir).map_err(SelectError::LaunchFailed)?;

    // Clear frames from an earlier run. Without this, a second extraction into
    // the same cache folder leaves stale PNGs behind, the file count stops
    // matching the timestamp count, and the run fails for a reason that has
    // nothing to do with the video.
    //
    // Only our own `frame-*.png` are touched: `--out` may point at a folder the
    // user cares about, and deleting anything else there would be unforgivable.
    remove_previous_frames(out_dir).map_err(SelectError::LaunchFailed)?;

    let pattern = out_dir.join("frame-%05d.png");

    // Array arguments throughout. `video` and `pattern` are single elements, so
    // spaces and non-ASCII characters never meet a shell.
    let args: Vec<OsString> = vec![
        "-hide_banner".into(),
        "-nostdin".into(),
        "-i".into(),
        video.as_os_str().to_owned(),
        "-vf".into(),
        params.filter_chain(hdr).into(),
        "-fps_mode".into(),
        "vfr".into(),
        "-f".into(),
        "image2".into(),
        "-y".into(),
        pattern.into_os_string(),
    ];

    let out = tools.run_ffmpeg(&args).map_err(SelectError::LaunchFailed)?;
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();

    if !out.status.success() {
        return Err(SelectError::FfmpegFailed { stderr });
    }

    let timestamps = parse_showinfo(&stderr);

    let mut files: Vec<PathBuf> = std::fs::read_dir(out_dir)
        .map_err(SelectError::LaunchFailed)?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| is_our_frame(p))
        .collect();
    files.sort();

    if files.is_empty() {
        return Err(SelectError::NoFramesWritten { stderr });
    }
    if files.len() != timestamps.len() {
        return Err(SelectError::CountMismatch {
            timestamps: timestamps.len(),
            files: files.len(),
        });
    }

    Ok(timestamps
        .into_iter()
        .zip(files)
        .map(|(pts_time, file)| SelectedFrame { pts_time, file })
        .collect())
}

/// Is this one of ours? `frame-<digits>.png` and nothing else.
///
/// Used for both cleanup and counting, and they have to agree. When only the
/// cleanup was scoped, an unrelated `.png` sitting in `--out` was still counted
/// as an extracted frame, the count stopped matching the timestamps, and the
/// run failed for a reason that had nothing to do with the video.
fn is_our_frame(path: &Path) -> bool {
    path.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
        n.starts_with("frame-")
            && n.ends_with(".png")
            && n.len() > "frame-.png".len()
            && n["frame-".len()..n.len() - ".png".len()]
                .chars()
                .all(|c| c.is_ascii_digit())
    })
}

/// Deletes only files this module wrote.
fn remove_previous_frames(dir: &Path) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if is_our_frame(&path) {
            std::fs::remove_file(&path)?;
        }
    }
    Ok(())
}

/// Pulls `pts_time` out of showinfo's stderr lines, in order.
///
/// A line looks like:
/// `[Parsed_showinfo_1 @ 0x..] n:   0 pts:      0 pts_time:0       duration: ...`
fn parse_showinfo(stderr: &str) -> Vec<f64> {
    stderr
        .lines()
        .filter(|l| l.contains("Parsed_showinfo"))
        .filter_map(|l| {
            let rest = l.split("pts_time:").nth(1)?;
            let value: String = rest
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '-')
                .collect();
            value.parse().ok()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_frame_is_always_selected() {
        // prev_selected_t is NaN until something is chosen; without this branch
        // the opening frame -- the one that says what the user was looking at --
        // would be dropped.
        assert!(SelectParams::default()
            .filter_expr()
            .contains("isnan(prev_selected_t),1"));
    }

    #[test]
    fn max_gap_must_exceed_min_gap() {
        let p = SelectParams {
            threshold: 0.12,
            min_gap: 5.0,
            max_gap: 2.0,
        };
        let err = p.validate().unwrap_err();
        assert!(err.contains("must be larger"), "got: {err}");
    }

    /// The default has to sit below where real screen changes score, or the
    /// scene gate never fires and max-gap quietly does everything -- which is
    /// the fixed-rate sampling this whole module exists to replace.
    #[test]
    fn default_threshold_is_below_a_real_screen_change() {
        const MEASURED_SCREEN_CHANGE: f64 = 0.0147;
        const MEASURED_SCROLL: f64 = 0.0780;
        let d = SelectParams::default();
        assert!(
            d.threshold < MEASURED_SCREEN_CHANGE,
            "scene gate would never fire"
        );
        assert!(
            d.threshold < MEASURED_SCROLL,
            "scrolling outscores content, so min-gap -- not the threshold -- has to hold it back"
        );
        assert!(
            d.min_gap >= 5.0,
            "min-gap is the only lever that bounds continuous scrolling"
        );
    }

    #[test]
    fn rejects_thresholds_outside_the_measured_range() {
        let err = SelectParams {
            threshold: 0.9,
            ..Default::default()
        }
        .validate()
        .unwrap_err();
        assert!(
            err.contains("0.015"),
            "should quote what a real screen change scores: {err}"
        );
    }

    #[test]
    fn hdr_sources_get_tone_mapped_before_selection() {
        // Order matters: scene scores must be computed on the same picture that
        // gets written, or we are measuring a different video than we save.
        let chain = SelectParams::default().filter_chain(true);
        let tone_at = chain.find("tonemap").expect("HDR chain must tone-map");
        let select_at = chain.find("select=").expect("chain must still select");
        assert!(
            tone_at < select_at,
            "tone-mapping has to come first:\n{chain}"
        );
    }

    #[test]
    fn sdr_sources_are_left_alone() {
        // Tone-mapping an ordinary screen recording would damage a perfectly
        // good picture.
        let chain = SelectParams::default().filter_chain(false);
        assert!(
            !chain.contains("tonemap"),
            "SDR must not be tone-mapped:\n{chain}"
        );
        assert!(!chain.contains("zscale"));
    }

    #[test]
    fn parses_showinfo_timestamps_in_order() {
        let stderr = "\
[Parsed_showinfo_1 @ 0x1] n:   0 pts:      0 pts_time:0       duration: 1
[Parsed_showinfo_1 @ 0x1] n:   1 pts:  30720 pts_time:2.5     duration: 1
some unrelated ffmpeg chatter
[Parsed_showinfo_1 @ 0x1] n:   2 pts:  61440 pts_time:12.008  duration: 1";
        assert_eq!(parse_showinfo(stderr), vec![0.0, 2.5, 12.008]);
    }

    #[test]
    fn ignores_stderr_without_showinfo_lines() {
        assert!(parse_showinfo("frame=  12 fps=0.0 q=-1.0 size=N/A").is_empty());
    }

    /// `--out` may be a folder the user cares about. Clearing a previous run
    /// must never reach beyond the files this module wrote.
    #[test]
    fn cleanup_touches_only_our_own_frames() {
        let dir = std::env::temp_dir().join("framekeep-cleanup-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let ours = dir.join("frame-00007.png");
        let theirs = dir.join("holiday.png");
        let notes = dir.join("frame-notes.png"); // ours-looking, but not numbered
        for p in [&ours, &theirs, &notes] {
            std::fs::write(p, b"x").unwrap();
        }

        remove_previous_frames(&dir).unwrap();

        assert!(!ours.exists(), "our own frame should be cleared");
        assert!(theirs.exists(), "an unrelated file must survive");
        assert!(notes.exists(), "a non-numbered name is not ours to delete");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Cleanup and counting must use the same rule. When they disagreed, an
    /// unrelated `.png` in `--out` was counted as an extracted frame and the
    /// run failed with a mismatch that had nothing to do with the video.
    #[test]
    fn ownership_rule_is_the_same_for_counting_and_cleanup() {
        assert!(is_our_frame(Path::new("frame-00001.png")));
        assert!(!is_our_frame(Path::new("holiday.png")));
        assert!(!is_our_frame(Path::new("frame-notes.png")));
        assert!(!is_our_frame(Path::new("frame-.png")));
        assert!(!is_our_frame(Path::new("frame-00001.jpg")));
    }
}
