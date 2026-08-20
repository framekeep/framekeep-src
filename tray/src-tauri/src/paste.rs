//! What a paste means. S4.3, S4.4, S4.5.
//!
//! Separate from `clipboard.rs` on purpose: that file is Windows FFI and can
//! only be exercised on a machine with a clipboard, while everything worth
//! arguing about -- is this a video, is that an image, what do we say when it
//! is neither -- is a decision over plain values, and gets tested everywhere.
//!
//! Every answer here is a sentence from `_design_system/copy.md`. A paste that
//! cannot be used is not an error state to be apologised for; it is a normal
//! thing a person did, answered plainly.

use crate::clipboard::Clipboard;
use std::path::{Path, PathBuf};

/// Containers `framekeep-core` can open. Extensions only -- this is a first
/// filter, not a verdict. `core` decodes the file and reports properly if it
/// turns out to be something else; guessing harder here would just be a second
/// opinion nobody asked for.
const VIDEO: [&str; 8] = ["mp4", "mov", "mkv", "webm", "m4v", "avi", "wmv", "mpg"];

const IMAGE: [&str; 8] = ["png", "jpg", "jpeg", "gif", "webp", "bmp", "tif", "tiff"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Hand these to the queue.
    Ingest(Vec<PathBuf>),
    /// Nothing to do, and a sentence explaining why. Never an error dialog:
    /// pasting the wrong thing is not a failure, it is a person trying.
    Nothing(String),
}

/// Decide what to do with what came off the clipboard.
///
/// `exists` is injected so the whole table can be tested without touching a
/// filesystem -- and so a test cannot accidentally depend on this machine.
pub fn decide(clipboard: &Clipboard, exists: &dyn Fn(&Path) -> bool) -> Outcome {
    match clipboard {
        Clipboard::Files(paths) => decide_files(paths, exists),

        // A path someone copied as text -- from a terminal, a chat message, a
        // file manager's address bar. Quotes come along often enough to be
        // worth handling rather than blaming the user for.
        Clipboard::Text(text) => {
            let trimmed = text.trim().trim_matches('"').trim_matches('\'');
            if trimmed.is_empty() {
                return Outcome::Nothing(NOTHING_USABLE.to_string());
            }
            let path = PathBuf::from(trimmed);
            if exists(&path) {
                return decide_files(std::slice::from_ref(&path), exists);
            }
            if looks_like_a_path(trimmed) {
                // It was meant as a path, so say what went wrong with *it*
                // rather than pretending nothing was on the clipboard.
                return Outcome::Nothing(format!(
                    "Couldn't find {}. Check the path, or drop the file in instead.",
                    file_name(&path)
                ));
            }
            Outcome::Nothing(NOTHING_USABLE.to_string())
        }

        // Copy on the clipboard as pixels. Measured 17/08 as CF_BITMAP +
        // CF_DIB + CF_DIBV5 -- and never as video: nothing standard on Windows
        // carries video bytes, so there is no "raw video data" case to write.
        Clipboard::Image => Outcome::Nothing(
            "That's an image — you can send it straight to your chat. Framekeep is for video."
                .to_string(),
        ),

        Clipboard::Empty => Outcome::Nothing(NOTHING_USABLE.to_string()),
    }
}

const NOTHING_USABLE: &str = "Nothing to paste. Copy a recording, then try again.";

/// Public because dropping files on the window asks the same question with
/// the same answers -- one decision table, or paste and drop would drift.
pub fn decide_files(paths: &[PathBuf], exists: &dyn Fn(&Path) -> bool) -> Outcome {
    let videos: Vec<PathBuf> = paths
        .iter()
        .filter(|p| has_extension(p, &VIDEO) && exists(p))
        .cloned()
        .collect();

    if !videos.is_empty() {
        return Outcome::Ingest(videos);
    }

    // Nothing usable: say which of the several reasons it was.
    if paths.iter().any(|p| has_extension(p, &IMAGE)) {
        return Outcome::Nothing(
            "That's an image — you can send it straight to your chat. Framekeep is for video."
                .to_string(),
        );
    }
    if let Some(missing) = paths
        .iter()
        .find(|p| has_extension(p, &VIDEO) && !exists(p))
    {
        return Outcome::Nothing(format!(
            "Couldn't find {}. It may have been moved or deleted.",
            file_name(missing)
        ));
    }
    Outcome::Nothing(
        "That file isn't a video Framekeep can read. Try MP4, MOV, or WebM.".to_string(),
    )
}

/// Is this a container `framekeep-core` can open? Shared with the folder
/// watcher, so a format this product accepts by paste is one it accepts by
/// import -- two lists would eventually disagree.
pub fn is_video(path: &Path) -> bool {
    has_extension(path, &VIDEO)
}

fn has_extension(path: &Path, list: &[&str]) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .is_some_and(|e| list.contains(&e.as_str()))
}

/// Text that was meant as a path, even though nothing is there.
///
/// Deliberately loose: it only decides which of two sentences to show.
fn looks_like_a_path(text: &str) -> bool {
    if text.lines().count() > 1 {
        return false;
    }
    let windows_drive = text.len() > 2 && text.as_bytes()[1] == b':';
    windows_drive || text.starts_with('\\') || text.starts_with('/') || text.starts_with('~')
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_exist(_: &Path) -> bool {
        true
    }
    fn none_exist(_: &Path) -> bool {
        false
    }

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn a_copied_recording_goes_straight_in() {
        let clip = Clipboard::Files(vec![p(r"C:\Users\A\Videos\demo.mp4")]);
        assert_eq!(
            decide(&clip, &all_exist),
            Outcome::Ingest(vec![p(r"C:\Users\A\Videos\demo.mp4")])
        );
    }

    /// The mandatory Unicode-and-spaces case, one layer up from ffmpeg.
    #[test]
    fn unicode_and_spaces_in_the_path_are_not_special() {
        for path in [
            r"C:\Users\Nguyễn Văn A\Videos\test.mp4",
            r"C:\Users\A\My Videos\screen rec.mov",
        ] {
            assert_eq!(
                decide(&Clipboard::Files(vec![p(path)]), &all_exist),
                Outcome::Ingest(vec![p(path)]),
                "{path}"
            );
        }
    }

    #[test]
    fn several_files_at_once_keep_only_the_videos() {
        let clip = Clipboard::Files(vec![
            p(r"C:\a\notes.txt"),
            p(r"C:\a\one.mp4"),
            p(r"C:\a\two.MOV"),
        ]);
        assert_eq!(
            decide(&clip, &all_exist),
            Outcome::Ingest(vec![p(r"C:\a\one.mp4"), p(r"C:\a\two.MOV")]),
            "extension matching has to be case-insensitive"
        );
    }

    /// S4.5. Not an error, and it does not scold.
    #[test]
    fn a_pasted_image_gets_a_civil_answer() {
        for clip in [
            Clipboard::Image,
            Clipboard::Files(vec![p(r"C:\a\shot.png")]),
        ] {
            let Outcome::Nothing(message) = decide(&clip, &all_exist) else {
                panic!("an image should not be ingested");
            };
            assert!(
                message.contains("send it straight to your chat"),
                "{message}"
            );
            assert!(!message.to_lowercase().contains("error"), "{message}");
        }
    }

    #[test]
    fn a_path_copied_as_text_works_the_same_as_a_copied_file() {
        let clip = Clipboard::Text(r"C:\Users\A\Videos\demo.mp4".to_string());
        assert_eq!(
            decide(&clip, &all_exist),
            Outcome::Ingest(vec![p(r"C:\Users\A\Videos\demo.mp4")])
        );
    }

    #[test]
    fn quotes_around_a_copied_path_are_not_the_users_problem() {
        // What a terminal's "copy as path" gives you.
        let clip = Clipboard::Text("\"C:\\Users\\A\\Videos\\demo.mp4\"".to_string());
        assert_eq!(
            decide(&clip, &all_exist),
            Outcome::Ingest(vec![p(r"C:\Users\A\Videos\demo.mp4")])
        );
    }

    #[test]
    fn a_path_that_is_not_there_says_so_about_that_path() {
        let clip = Clipboard::Text(r"C:\gone\missing.mp4".to_string());
        let Outcome::Nothing(message) = decide(&clip, &none_exist) else {
            panic!("a missing file cannot be ingested");
        };
        assert!(message.contains("missing.mp4"), "{message}");
        assert!(message.contains("Couldn't find"), "{message}");
    }

    #[test]
    fn ordinary_text_does_not_pretend_to_be_a_broken_path() {
        let clip = Clipboard::Text("remember to fix the payment flow".to_string());
        let Outcome::Nothing(message) = decide(&clip, &none_exist) else {
            panic!("prose is not a recording");
        };
        assert_eq!(message, NOTHING_USABLE);
    }

    #[test]
    fn a_file_we_cannot_open_names_what_we_can() {
        let clip = Clipboard::Files(vec![p(r"C:\a\archive.zip")]);
        let Outcome::Nothing(message) = decide(&clip, &all_exist) else {
            panic!("a zip is not a recording");
        };
        assert!(message.contains("MP4, MOV, or WebM"), "{message}");
    }

    #[test]
    fn an_empty_clipboard_is_answered_not_ignored() {
        let Outcome::Nothing(message) = decide(&Clipboard::Empty, &all_exist) else {
            panic!("nothing to ingest");
        };
        assert_eq!(message, NOTHING_USABLE);
    }

    /// Every sentence this module can produce, checked against the house voice
    /// in copy.md: no exclamation marks, no `please`, no `I`, no raw exception
    /// text, and something to do next.
    #[test]
    fn every_answer_sounds_like_the_product() {
        let cases = [
            decide(&Clipboard::Empty, &all_exist),
            decide(&Clipboard::Image, &all_exist),
            decide(&Clipboard::Text("hello".into()), &none_exist),
            decide(&Clipboard::Text(r"C:\gone.mp4".into()), &none_exist),
            decide(&Clipboard::Files(vec![p(r"C:\a\x.zip")]), &all_exist),
            decide(&Clipboard::Files(vec![p(r"C:\a\x.mp4")]), &none_exist),
        ];
        for case in cases {
            let Outcome::Nothing(message) = case else {
                continue;
            };
            assert!(!message.contains('!'), "{message}");
            assert!(!message.to_lowercase().contains("please"), "{message}");
            assert!(!message.contains(" I "), "{message}");
            assert!(!message.starts_with("Error"), "{message}");
            assert!(message.ends_with('.'), "a sentence, finished: {message}");
        }
    }
}
