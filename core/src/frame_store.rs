//! Remembering which frames a recording was already broken into.
//!
//! # Why
//!
//! Extraction is not free -- 2.3 seconds for a two-minute SDR recording, about
//! ten times that for HDR, because tone-mapping costs 6.4x. And it was being
//! paid again on every call: the MCP adapter runs `map` to list the frames, then
//! runs it again to fetch some of them, and a caller who asks for three ranges
//! pays four times for one piece of work.
//!
//! The frames are already sitting in the cache folder. This records what they
//! are, so the second call can look instead of redo.
//!
//! # What makes a cached list valid
//!
//! The selection parameters, and nothing else. A different threshold or gap
//! produces a different set of frames, so a cached list from other parameters is
//! not a cheaper answer -- it is the wrong answer. The recording itself is
//! already covered: the cache folder is keyed by a handle derived from path,
//! size and modification time, so an edited video lands somewhere else entirely.
//!
//! Every file is checked to still exist before the list is trusted. A user who
//! cleaned out the folder should get fresh frames, not a list of paths to
//! nothing.

use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::encode::Format;
use crate::select::{SelectParams, SelectedFrame};

const FILE: &str = "frames.json";

#[derive(Debug, Serialize, Deserialize)]
struct Stored {
    threshold: f64,
    min_gap: f64,
    max_gap: f64,
    /// Part of the key, not decoration: frames written as PNG cannot answer a
    /// request for WebP.
    format: String,
    created_unix: u64,
    /// How many frames the selector produced before dedup, and how many dedup
    /// dropped. Stored so a cache hit reports the same summary a fresh run
    /// would -- a number that changes depending on whether the cache happened
    /// to be warm is a number nobody can use.
    selected: usize,
    dropped: usize,
    frames: Vec<StoredFrame>,
}

/// What a previous run produced.
pub struct Cached {
    pub frames: Vec<SelectedFrame>,
    pub selected: usize,
    pub dropped: usize,
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredFrame {
    pts_time: f64,
    /// **Relative to the folder holding this index.** The frames are always in
    /// it, so the folder is the one thing that never needs writing down.
    ///
    /// It used to be absolute, and that made the cache un-relocatable in a way
    /// that failed silently. Moving `~/.framekeep` left every path stale, core
    /// re-extracted (slow but correct), and the tray -- which drops frames
    /// whose file is missing -- reported a twenty-frame recording as having
    /// one. No error anywhere: a caller cannot tell a moved cache from a short
    /// recording.
    ///
    /// Old indexes stay readable without a migration, because joining a folder
    /// with an absolute path yields the absolute path. Nothing has to detect
    /// which kind it is holding.
    file: String,
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Floating-point parameters are compared with a tolerance rather than for
/// equality: these arrive as text on a command line and round-trip through
/// JSON, and `0.012` deserves to match `0.012`.
fn same(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

/// The frames from a previous run, if they answer the same question.
///
/// `None` means "extract them" -- no cache, different parameters, or files that
/// have since been deleted.
pub fn load(dir: &Path, params: &SelectParams, format: Format) -> Option<Cached> {
    let bytes = std::fs::read(dir.join(FILE)).ok()?;
    let stored: Stored = serde_json::from_slice(&bytes).ok()?;

    if !same(stored.threshold, params.threshold)
        || !same(stored.min_gap, params.min_gap)
        || !same(stored.max_gap, params.max_gap)
        || stored.format != format.extension()
    {
        return None;
    }

    let (selected, dropped) = (stored.selected, stored.dropped);
    let frames: Vec<SelectedFrame> = stored
        .frames
        .into_iter()
        .map(|f| SelectedFrame {
            pts_time: f.pts_time,
            // Joining handles both spellings: a relative name lands in `dir`,
            // and an absolute one from an older index replaces it outright.
            file: dir.join(f.file),
        })
        .collect();

    // A list of paths to files that are gone is worse than no list: it turns a
    // slow call into a broken one.
    if frames.is_empty() || !frames.iter().all(|f| f.file.is_file()) {
        return None;
    }
    Some(Cached {
        frames,
        selected,
        dropped,
    })
}

/// Records what was just extracted. Best effort -- failing to write this costs
/// time on the next call, not correctness on this one.
pub fn save(
    dir: &Path,
    params: &SelectParams,
    format: Format,
    frames: &[SelectedFrame],
    selected: usize,
    dropped: usize,
) {
    let stored = Stored {
        threshold: params.threshold,
        min_gap: params.min_gap,
        max_gap: params.max_gap,
        format: format.extension().to_string(),
        created_unix: now_unix(),
        selected,
        dropped,
        frames: frames
            .iter()
            .map(|f| StoredFrame {
                pts_time: f.pts_time,
                // Relative to `dir`. `strip_prefix` is the honest form; the
                // fallback covers a caller that handed us a path from
                // somewhere else, which no current one does.
                file: f
                    .file
                    .strip_prefix(dir)
                    .unwrap_or(&f.file)
                    .display()
                    .to_string(),
            })
            .collect(),
    };
    if let Ok(bytes) = serde_json::to_vec_pretty(&stored) {
        let partial = dir.join("frames.partial");
        if std::fs::write(&partial, &bytes).is_ok()
            && std::fs::rename(&partial, dir.join(FILE)).is_err()
        {
            let _ = std::fs::remove_file(&partial);
        }
    }
}

/// Forgets the cached list. Called before a fresh extraction, so a crash midway
/// cannot leave a list describing frames that were half-replaced.
pub fn clear(dir: &Path) {
    let _ = std::fs::remove_file(dir.join(FILE));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "framekeep-frames-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn write_frames(dir: &Path, n: usize) -> Vec<SelectedFrame> {
        (0..n)
            .map(|i| {
                let file = dir.join(format!("frame-{i:03}.png"));
                std::fs::write(&file, b"x").unwrap();
                SelectedFrame {
                    pts_time: i as f64,
                    file,
                }
            })
            .collect()
    }

    #[test]
    fn the_same_question_gets_the_cached_answer() {
        let dir = temp("hit");
        let p = SelectParams::default();
        let frames = write_frames(&dir, 3);
        save(&dir, &p, Format::Png, &frames, 5, 2);

        let got = load(&dir, &p, Format::Png).expect("same parameters should hit");
        assert_eq!(got.frames.len(), 3);
        assert_eq!(got.frames[0].pts_time, 0.0);
        // The summary must survive too, or the same run reports different
        // numbers depending on whether the cache happened to be warm.
        assert_eq!((got.selected, got.dropped), (5, 2));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn different_parameters_are_a_different_question() {
        let dir = temp("params");
        let p = SelectParams::default();
        save(&dir, &p, Format::Png, &write_frames(&dir, 2), 2, 0);

        // A cached list from another threshold is not a cheaper answer, it is
        // the wrong one.
        let other = SelectParams {
            threshold: p.threshold * 2.0,
            ..p
        };
        assert!(load(&dir, &other, Format::Png).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_different_format_cannot_be_served_from_the_cache() {
        let dir = temp("format");
        let p = SelectParams::default();
        save(&dir, &p, Format::Png, &write_frames(&dir, 2), 2, 0);
        assert!(load(&dir, &p, Format::Webp).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_list_pointing_at_deleted_files_is_not_used() {
        let dir = temp("gone");
        let p = SelectParams::default();
        let frames = write_frames(&dir, 2);
        save(&dir, &p, Format::Png, &frames, 2, 0);

        std::fs::remove_file(&frames[1].file).unwrap();
        // Serving this would turn a slow call into a broken one.
        assert!(load(&dir, &p, Format::Png).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clearing_makes_the_next_call_extract_again() {
        let dir = temp("clear");
        let p = SelectParams::default();
        save(&dir, &p, Format::Png, &write_frames(&dir, 2), 2, 0);
        clear(&dir);
        assert!(load(&dir, &p, Format::Png).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
