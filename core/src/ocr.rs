//! Reading text off a frame with `Windows.Media.Ocr`. S5.1.
//!
//! The engine choice was settled by measurement before any of this was written
//! (`docs/experiments/ocr-windows-media.md`): 0 MB to ship, ~42 ms a frame, no
//! false positives, per-word boxes -- and 83% recall at 16px text falling off a
//! cliff to 8% at 11px. That last number is why nothing downstream may describe
//! redaction as complete, and why a person reviews every frame.
//!
//! # Why this returns spans as well as boxes
//!
//! `secrets::scan` works on a string and reports byte ranges into it. Masking
//! needs pixels. The measurement found two cases where a pattern matched but no
//! box lined up with it -- `found` running ahead of `located`.
//!
//! That gap closes by construction rather than by heuristic: this module builds
//! the string itself, out of the words, recording each word's byte range as it
//! goes. Every span then maps back to the words it covers, exactly. What is
//! left is the honest case -- a match spanning words that sit far apart on
//! screen -- and callers can see it, because they get one rect per word and can
//! judge the geometry instead of being handed a bounding box that swallows
//! whatever lies between.

use serde::Serialize;
use std::fmt;
use std::ops::Range;

/// A rectangle in frame pixels, matching `OcrWord.BoundingRect`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// Why a frame produced no reading.
///
/// Two variants rather than one string, because callers act on the difference:
/// no engine is a property of the *machine* and every remaining frame will fail
/// the same way, while a failed read is a property of *this file* and the next
/// one may be fine. Sniffing that distinction out of an error message is how a
/// scan of fifteen frames ends up reporting "clean" fifteen times.
#[derive(Debug)]
pub enum Error {
    /// No OCR language pack for this machine's languages -- or not Windows.
    NoEngine(String),
    /// The engine exists; this image did not survive it.
    Failed(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NoEngine(m) | Error::Failed(m) => write!(f, "{m}"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Word {
    pub rect: Rect,
    /// Where this word sits in `Reading::text`. The word itself is not stored
    /// again alongside it: `&reading.text[word.span]` is the same characters,
    /// and two copies of one string is two places for it to disagree.
    pub span: Range<usize>,
}

/// One frame, as the engine read it.
#[derive(Debug, Clone)]
pub struct Reading {
    /// Every word joined with single spaces. This is what gets scanned, and
    /// it is flat on purpose: the engine's own line breaks are a guess about
    /// layout, and a secret split across two of them is still a secret.
    pub text: String,
    pub words: Vec<Word>,
    /// e.g. `en-US`. Recorded because recall is language-dependent and a
    /// finding is only as good as the engine that produced it.
    pub language: String,
}

impl Reading {
    /// Build a reading from words in reading order.
    ///
    /// The joining rule lives here, alone, because the scanner's measured
    /// recall depends on it: the fixtures in `core/tests/fixtures/ocr/` were
    /// frozen from WinRT's own `OcrResult.Text`, and that string turned out to
    /// be exactly the words joined by single spaces (checked against the engine
    /// on a real frame -- 47 words, byte-identical). Join them any other way --
    /// newlines between lines, say -- and every number measured for
    /// `secrets.rs` quietly stops describing this code.
    pub fn from_words(
        language: String,
        words: impl IntoIterator<Item = (String, Rect)>,
    ) -> Reading {
        let mut text = String::new();
        let mut out = Vec::new();
        for (word, rect) in words {
            if word.is_empty() {
                continue;
            }
            if !text.is_empty() {
                text.push(' ');
            }
            let start = text.len();
            text.push_str(&word);
            out.push(Word {
                rect,
                span: start..text.len(),
            });
        }
        Reading {
            text,
            words: out,
            language,
        }
    }

    /// The word boxes covering a byte range of `text`.
    ///
    /// One rect per word, never a union: a match that spans two words at
    /// opposite ends of the screen would produce a union rectangle covering
    /// everything between them, and painting over content nobody asked to hide
    /// is its own kind of broken.
    ///
    /// An empty result is the `found`-but-not-`located` case. It is a real
    /// outcome, not a bug, and callers must surface it rather than skip it --
    /// a secret that was detected and silently left visible is the worst
    /// available outcome.
    pub fn boxes_for(&self, span: &Range<usize>) -> Vec<Rect> {
        self.words
            .iter()
            .filter(|w| w.span.start < span.end && span.start < w.span.end)
            .map(|w| w.rect)
            .collect()
    }
}

#[cfg(windows)]
mod engine {
    use super::{Error, Reading, Rect};
    use windows::core::RuntimeType;
    use windows::Graphics::Imaging::{BitmapAlphaMode, BitmapDecoder, BitmapPixelFormat};
    use windows::Media::Ocr::OcrEngine;
    use windows::Storage::Streams::{DataWriter, InMemoryRandomAccessStream};
    use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
    use windows_future::{AsyncOperationCompletedHandler, IAsyncOperation};

    /// Wait for a WinRT async operation and take its result.
    ///
    /// `windows-future` has exactly this as `Async::join`, and its own readme
    /// shows `use windows_future::*;` then `.join()` -- but in 0.3.2 that trait
    /// is imported privately in the crate's `lib.rs`, so it never leaves the
    /// crate and the documented call will not compile. This is the same wait
    /// through the surface that is actually public.
    ///
    /// No polling loop: WinRT calls the completion handler immediately when the
    /// operation has already finished, so subscribing after starting is not a
    /// race.
    fn block_on<T: RuntimeType>(op: IAsyncOperation<T>) -> windows::core::Result<T> {
        let (done, wait) = std::sync::mpsc::channel();
        op.SetCompleted(&AsyncOperationCompletedHandler::new(move |_, _| {
            let _ = done.send(());
            Ok(())
        }))?;
        let _ = wait.recv();
        op.GetResults()
    }

    /// Read a PNG (or anything `BitmapDecoder` accepts) that is already in
    /// memory.
    ///
    /// Bytes rather than a path, and so `StorageFile` never enters the picture.
    /// That API goes through the file broker, which brings its own rules about
    /// which paths a packaged app may name -- and S6 packages this app. Handing
    /// the decoder bytes we already read sidesteps the question entirely, and
    /// keeps `C:\Users\Nguyễn Văn A\Videos\` a problem for the file reader
    /// rather than a problem for OCR.
    pub fn read(png: &[u8]) -> Result<Reading, Error> {
        // WinRT activation needs an initialised apartment, and the failure
        // without one is `CO_E_NOTINITIALIZED` from a call that mentions
        // neither COM nor threads. Already-initialised is not an error here:
        // this is a CLI that may be called from anywhere.
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }

        let engine = OcrEngine::TryCreateFromUserProfileLanguages().map_err(|e| {
            Error::NoEngine(format!(
                "no OCR engine for this machine's languages ({e}). Windows ships OCR \
                 language packs separately from the language itself -- on Server editions \
                 they are a Feature on Demand and are not installed by default."
            ))
        })?;

        let stream = InMemoryRandomAccessStream::new().map_err(win)?;
        let writer = DataWriter::CreateDataWriter(&stream).map_err(win)?;
        writer.WriteBytes(png).map_err(win)?;
        block_on(writer.StoreAsync().map_err(win)?).map_err(win)?;
        block_on(writer.FlushAsync().map_err(win)?).map_err(win)?;
        // Detach, or dropping the writer closes the stream underneath us.
        writer.DetachStream().map_err(win)?;
        stream.Seek(0).map_err(win)?;

        let decoder = block_on(BitmapDecoder::CreateAsync(&stream).map_err(win)?).map_err(win)?;

        // The engine wants Bgra8. Converting here rather than letting it
        // complain about a format, which is a message that helps nobody.
        let bitmap = block_on(
            decoder
                .GetSoftwareBitmapConvertedAsync(
                    BitmapPixelFormat::Bgra8,
                    BitmapAlphaMode::Premultiplied,
                )
                .map_err(win)?,
        )
        .map_err(win)?;

        let result = block_on(engine.RecognizeAsync(&bitmap).map_err(win)?).map_err(win)?;

        let language = engine
            .RecognizerLanguage()
            .and_then(|l| l.LanguageTag())
            .map(|t| t.to_string())
            .unwrap_or_default();

        // Flattened across lines on purpose. The engine's line breaks are a
        // guess about layout, and a key split across two of them is still a key.
        let mut words = Vec::new();
        for line in result.Lines().map_err(win)? {
            for word in line.Words().map_err(win)? {
                let rect = word.BoundingRect().map_err(win)?;
                words.push((
                    word.Text().map_err(win)?.to_string(),
                    Rect {
                        x: rect.X,
                        y: rect.Y,
                        w: rect.Width,
                        h: rect.Height,
                    },
                ));
            }
        }

        Ok(Reading::from_words(language, words))
    }

    fn win(e: windows::core::Error) -> Error {
        Error::Failed(format!("OCR failed: {e}"))
    }
}

#[cfg(not(windows))]
mod engine {
    use super::{Error, Reading};

    pub fn read(_png: &[u8]) -> Result<Reading, Error> {
        // Not `unimplemented!()`: a panic here would read as a crash. macOS
        // gets Apple Vision in S7, and until then this is a plain answer --
        // and `NoEngine` is the honest variant, because every frame on this
        // machine will say the same thing.
        Err(Error::NoEngine(
            "OCR is Windows-only in this build (macOS uses Apple Vision, slice S7)".into(),
        ))
    }
}

pub use engine::read;

#[cfg(test)]
mod tests {
    use super::*;

    /// Built through the real constructor, so these tests exercise the joining
    /// rule rather than a second copy of it that could drift.
    fn reading(words: &[(&str, f32)]) -> Reading {
        Reading::from_words(
            "en-US".into(),
            words.iter().map(|(w, x)| {
                (
                    (*w).to_string(),
                    Rect {
                        x: *x,
                        y: 100.0,
                        w: 40.0,
                        h: 16.0,
                    },
                )
            }),
        )
    }

    #[test]
    fn words_are_joined_by_exactly_one_space_and_nothing_else() {
        // `secrets.rs` measured 83/62/8 percent against readings frozen from
        // WinRT's `OcrResult.Text`, which is this same string. Change the
        // joining and those numbers silently stop describing this code, with
        // no test failing anywhere near the change -- so the rule is pinned
        // here, next to the constructor that owns it.
        let r = reading(&[("OPENAI", 10.0), ("KEY=abc", 60.0), ("tail", 200.0)]);
        assert_eq!(r.text, "OPENAI KEY=abc tail");
        assert!(
            r.text.lines().nth(1).is_none(),
            "line breaks would split a key in two"
        );
        assert!(
            !r.text.contains("  "),
            "a double space shifts every later span"
        );
    }

    #[test]
    fn an_empty_word_is_dropped_without_leaving_a_gap_in_the_text() {
        // The engine does emit these. Left in, each one would add a stray space
        // and push every following span off its box by one byte.
        let r = Reading::from_words(
            "en-US".into(),
            [
                (
                    "a".to_string(),
                    Rect {
                        x: 0.0,
                        y: 0.0,
                        w: 1.0,
                        h: 1.0,
                    },
                ),
                (
                    String::new(),
                    Rect {
                        x: 9.0,
                        y: 9.0,
                        w: 1.0,
                        h: 1.0,
                    },
                ),
                (
                    "b".to_string(),
                    Rect {
                        x: 2.0,
                        y: 0.0,
                        w: 1.0,
                        h: 1.0,
                    },
                ),
            ],
        );
        assert_eq!(r.text, "a b");
        assert_eq!(r.words.len(), 2);
        let at = r.text.find('b').unwrap();
        assert_eq!(r.boxes_for(&(at..at + 1))[0].x, 2.0);
    }

    #[test]
    fn every_span_of_the_text_maps_back_to_a_word() {
        let r = reading(&[
            ("OPENAI", 10.0),
            ("KEY=sk-proj-T3Blbk", 60.0),
            ("done", 200.0),
        ]);
        // This is the property the whole design rests on: the string was built
        // from the words, so anything a scanner points at is locatable.
        let at = r.text.find("sk-proj-T3Blbk").unwrap();
        let boxes = r.boxes_for(&(at..at + "sk-proj-T3Blbk".len()));
        assert_eq!(boxes.len(), 1);
        assert_eq!(boxes[0].x, 60.0);
    }

    #[test]
    fn a_match_spanning_two_words_yields_two_boxes_not_one_covering_both() {
        let r = reading(&[("STRIPE", 10.0), ("51H8xQ2LmNpR4tV7wY0zA3bC6", 900.0)]);
        let boxes = r.boxes_for(&(0..r.text.len()));
        assert_eq!(boxes.len(), 2);
        // Nothing here reports the gap between x=50 and x=900 as covered.
        assert!(boxes.iter().all(|b| b.w == 40.0));
    }

    #[test]
    fn a_span_past_the_end_locates_nothing_rather_than_panicking() {
        let r = reading(&[("hello", 10.0)]);
        assert!(r.boxes_for(&(100..120)).is_empty());
    }
}
