//! Removing frames that are provably the same as the one before them.
//!
//! The rule here is deliberately timid, and the reason is in `FOUNDATION.md`:
//! dropping a frame because we judged it uninteresting **is interpretation**,
//! and this tool is the eye, not the brain. A redundant frame costs tokens; a
//! dropped frame costs correctness. Those prices are not equal.
//!
//! An earlier plan used `pHash Hamming <= 4`. Measurement killed it: typing
//! three characters into a line of code moves the hash by **0**, and rewriting
//! a whole line moves it by exactly 4 -- the threshold itself. Nor does
//! switching to pixel measurement rescue it: the margin between "typed some
//! text" (keep) and "cursor blinked" (drop) is only 1.1-1.2x, which is noise.
//! See `docs/experiments/dedup-phash-screen-recording.md`.
//!
//! So v1 drops only what it can prove, and everything ambiguous survives. Frame
//! *count* is not this module's job -- the scene threshold, min-gap and max-gap
//! bound it above, and the byte budget bounds it below.
//!
//! # The seam that matters
//!
//! [`Deduper`] is one function: given the last frame we kept and a candidate,
//! keep or drop. The comparison channel is pluggable on purpose. When OCR
//! arrives for redaction, comparing *text* separates the two cases that pixels
//! cannot -- a blinking cursor produces zero characters, typing produces three
//! -- and that becomes a new implementation of this trait rather than a rewrite
//! of frame selection.

use std::fmt;
use std::path::Path;

/// What to do with a candidate frame, and why. The reason is carried rather
/// than discarded: a frame that vanishes without explanation is the worst kind
/// of bug -- no error, no warning, the model simply never sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Keep,
    Drop(&'static str),
}

/// Given the last frame we kept and a candidate, decide.
///
/// Implementations must be conservative: return [`Decision::Keep`] whenever the
/// evidence for dropping is not conclusive.
pub trait Deduper {
    fn decide(&self, last_kept: &Frame, candidate: &Frame) -> Decision;
    /// Named in output so a run can be explained after the fact.
    fn name(&self) -> &'static str;
}

/// A decoded frame, reduced to what comparison needs.
pub struct Frame {
    pub width: u32,
    pub height: u32,
    /// 8-bit luminance, row-major, `width * height` long.
    pub gray: Vec<u8>,
    /// 64-bit perceptual hash, from the DCT of a 32x32 reduction.
    pub phash: u64,
}

#[derive(Debug)]
pub enum DecodeError {
    Io(std::io::Error),
    NotPng { path: String, detail: String },
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::Io(e) => write!(f, "Couldn't read an extracted frame: {e}"),
            DecodeError::NotPng { path, detail } => write!(
                f,
                "An extracted frame isn't readable as PNG: {path}\n{detail}\n\
                 This is a bug in Framekeep -- frames are written by us, so they should always decode."
            ),
        }
    }
}

impl Frame {
    pub fn load(path: &Path) -> Result<Self, DecodeError> {
        let file = std::fs::File::open(path).map_err(DecodeError::Io)?;
        let decoder = png::Decoder::new(std::io::BufReader::new(file));
        let mut reader = decoder.read_info().map_err(|e| DecodeError::NotPng {
            path: path.display().to_string(),
            detail: e.to_string(),
        })?;

        let mut buf = vec![0; reader.output_buffer_size()];
        let info = reader
            .next_frame(&mut buf)
            .map_err(|e| DecodeError::NotPng {
                path: path.display().to_string(),
                detail: e.to_string(),
            })?;

        let channels = info.color_type.samples();
        let gray = to_gray(&buf[..info.buffer_size()], channels);
        let phash = phash64(&gray, info.width, info.height);

        Ok(Frame {
            width: info.width,
            height: info.height,
            gray,
            phash,
        })
    }
}

/// Rule A: drop only when the frames are the same by two independent measures.
///
/// Two conditions, not one. The hash alone is blind to small text changes -- the
/// experiment above proves it -- so the pixel check has to agree before
/// anything is discarded.
pub struct ProvablyIdentical {
    /// Mean absolute luminance difference, in 0-255 units, below which two
    /// frames count as the same picture. Small but non-zero, because decoding
    /// the same source frame twice out of a lossy codec is not bit-exact.
    ///
    /// Measured, not chosen -- see [`Self::default`].
    pub pixel_floor: f64,
}

impl Default for ProvablyIdentical {
    /// The floor is set by one number: the smallest edit a person can make.
    ///
    /// Measured on 1920x1080 frames (`docs/experiments/dedup-pixel-floor.py`):
    ///
    /// | change | mean difference |
    /// |---|---|
    /// | nothing at all | 0.0000 |
    /// | codec noise, h264 at 8 Mbit | 0.0032 |
    /// | **one character typed** | **0.0080** |
    /// | three characters typed | 0.0187 |
    ///
    /// So the floor has to sit under 0.0080 or it eats text. It previously sat
    /// at 0.5 -- sixty times too high -- and dropped both typing cases. The old
    /// unit test missed it by drawing on a 200x200 image, where the same edit
    /// is a hundred times louder than it is on a real screen.
    ///
    /// The two prices are not equal (`FOUNDATION.md`, principle I): a redundant
    /// frame costs tokens, a lost frame costs correctness. So the error is
    /// deliberately pushed to the cheap side -- 0.004 leaves 2x of room under a
    /// one-character edit, and only 1.25x over the best-case codec noise. On
    /// heavily compressed video the noise floor rises past this and dedup
    /// simply stops dropping anything. That is the correct way for it to fail.
    ///
    /// # What this rule gives up
    ///
    /// A blinking cursor measures **0.0141** -- heavier than the one-character
    /// edit it must not be confused with. The signal is inverted, exactly as it
    /// is for scene scores, so no floor drops the cursor while keeping typing.
    /// This rule keeps the cursor frame. Separating them needs the text channel
    /// (OCR), which is what the [`Deduper`] seam exists for.
    fn default() -> Self {
        ProvablyIdentical { pixel_floor: 0.004 }
    }
}

impl Deduper for ProvablyIdentical {
    fn decide(&self, last_kept: &Frame, candidate: &Frame) -> Decision {
        // Different geometry is a real change: a window resize is exactly the
        // kind of moment a screen recording exists to show.
        if last_kept.width != candidate.width || last_kept.height != candidate.height {
            return Decision::Keep;
        }
        if last_kept.phash != candidate.phash {
            return Decision::Keep;
        }
        if mean_abs_diff(&last_kept.gray, &candidate.gray) > self.pixel_floor {
            return Decision::Keep;
        }
        Decision::Drop("identical to the previous kept frame")
    }

    fn name(&self) -> &'static str {
        "provably-identical"
    }
}

fn mean_abs_diff(a: &[u8], b: &[u8]) -> f64 {
    debug_assert_eq!(a.len(), b.len());
    if a.is_empty() {
        return 0.0;
    }
    let total: u64 = a.iter().zip(b).map(|(x, y)| x.abs_diff(*y) as u64).sum();
    total as f64 / a.len() as f64
}

fn to_gray(buf: &[u8], channels: usize) -> Vec<u8> {
    match channels {
        1 => buf.to_vec(),
        _ => buf
            .chunks_exact(channels)
            .map(|px| {
                // Rec. 601 luma, in integer arithmetic.
                ((px[0] as u32 * 299 + px[1] as u32 * 587 + px[2] as u32 * 114) / 1000) as u8
            })
            .collect(),
    }
}

const DCT_N: usize = 32;
const HASH_N: usize = 8;

/// Standard pHash: reduce to 32x32, 2D DCT-II, keep the low-frequency 8x8
/// corner minus DC, threshold at the median.
fn phash64(gray: &[u8], width: u32, height: u32) -> u64 {
    if width == 0 || height == 0 || gray.is_empty() {
        return 0;
    }
    let small = box_downscale(gray, width as usize, height as usize, DCT_N);
    let coeffs = dct2d(&small);

    let mut low = Vec::with_capacity(HASH_N * HASH_N - 1);
    for y in 0..HASH_N {
        for x in 0..HASH_N {
            if x == 0 && y == 0 {
                continue; // DC term carries average brightness, not structure.
            }
            low.push(coeffs[y * DCT_N + x]);
        }
    }

    let mut sorted = low.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median = sorted[sorted.len() / 2];

    let mut hash = 0u64;
    for (i, v) in low.iter().enumerate() {
        if *v > median {
            hash |= 1 << i;
        }
    }
    hash
}

fn box_downscale(src: &[u8], w: usize, h: usize, n: usize) -> Vec<f64> {
    let mut out = vec![0.0; n * n];
    for oy in 0..n {
        let y0 = oy * h / n;
        let y1 = (((oy + 1) * h) / n).max(y0 + 1).min(h);
        for ox in 0..n {
            let x0 = ox * w / n;
            let x1 = (((ox + 1) * w) / n).max(x0 + 1).min(w);
            let mut sum = 0u64;
            let mut count = 0u64;
            for y in y0..y1 {
                for x in x0..x1 {
                    sum += src[y * w + x] as u64;
                    count += 1;
                }
            }
            out[oy * n + ox] = if count == 0 {
                0.0
            } else {
                sum as f64 / count as f64
            };
        }
    }
    out
}

fn dct2d(input: &[f64]) -> Vec<f64> {
    let n = DCT_N;
    let mut cos_table = vec![0.0; n * n];
    for k in 0..n {
        for i in 0..n {
            cos_table[k * n + i] =
                ((2 * i + 1) as f64 * k as f64 * std::f64::consts::PI / (2.0 * n as f64)).cos();
        }
    }

    let mut rows = vec![0.0; n * n];
    for y in 0..n {
        for k in 0..n {
            let mut sum = 0.0;
            for x in 0..n {
                sum += input[y * n + x] * cos_table[k * n + x];
            }
            rows[y * n + k] = sum;
        }
    }

    let mut out = vec![0.0; n * n];
    for x in 0..n {
        for k in 0..n {
            let mut sum = 0.0;
            for y in 0..n {
                sum += rows[y * n + x] * cos_table[k * n + y];
            }
            out[k * n + x] = sum;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, v: u8) -> Frame {
        let gray = vec![v; (w * h) as usize];
        let phash = phash64(&gray, w, h);
        Frame {
            width: w,
            height: h,
            gray,
            phash,
        }
    }

    #[test]
    fn identical_frames_are_dropped() {
        let d = ProvablyIdentical::default();
        assert_eq!(
            d.decide(&solid(64, 64, 128), &solid(64, 64, 128)),
            Decision::Drop("identical to the previous kept frame")
        );
    }

    #[test]
    fn a_resize_is_always_a_change() {
        // A window resize is exactly what a screen recording exists to show.
        let d = ProvablyIdentical::default();
        assert_eq!(
            d.decide(&solid(64, 64, 128), &solid(128, 64, 128)),
            Decision::Keep
        );
    }

    /// The case that killed `Hamming <= 4`: a few characters change inside an
    /// otherwise still frame. Rule A must keep it.
    #[test]
    fn a_small_text_change_survives() {
        let mut a = solid(200, 200, 240);
        let mut b = solid(200, 200, 240);
        // Draw a short dark run in b only, the size of a few characters.
        for x in 20..60 {
            for y in 100..112 {
                b.gray[y * 200 + x] = 20;
            }
        }
        a.phash = phash64(&a.gray, 200, 200);
        b.phash = phash64(&b.gray, 200, 200);

        let d = ProvablyIdentical::default();
        assert_eq!(
            d.decide(&a, &b),
            Decision::Keep,
            "a text-sized change must never be dropped -- this is the failure that killed the old rule"
        );
    }

    /// The same edit, at the size a screen actually is.
    ///
    /// `a_small_text_change_survives` above draws a 40x12 mark on a 200x200
    /// image: 1.2% of the picture. The identical edit on a 1920x1080 screen is
    /// 0.008% -- a hundred and fifty times quieter. That gap is the whole
    /// reason a floor of 0.5 looked fine in tests while eating typing in
    /// production, so the real frame size is part of this test.
    #[test]
    fn a_one_character_edit_on_a_real_sized_frame_survives() {
        const W: u32 = 1920;
        const H: u32 = 1080;
        let a = solid(W, H, 240);
        let mut b = solid(W, H, 240);

        // Reproduce the measured mean difference of a single typed character,
        // 0.0080: 0.0080 * 1920 * 1080 / (240 - 20) is about 75 pixels.
        for i in 0..75 {
            b.gray[500 * W as usize + 800 + i] = 20;
        }
        // The hash is blind to a change this small -- measured Hamming 0 -- so
        // fixing it here puts the pixel floor on trial, which is the point.
        b.phash = a.phash;

        assert_eq!(
            ProvablyIdentical::default().decide(&a, &b),
            Decision::Keep,
            "one typed character must survive dedup; the pixel floor is too high"
        );
    }

    #[test]
    fn faint_codec_noise_does_not_defeat_the_drop() {
        // Same picture decoded twice out of a lossy codec is not bit-exact.
        let a = solid(64, 64, 128);
        let mut b = solid(64, 64, 128);
        b.gray[0] = 129;
        b.phash = a.phash;
        assert!(matches!(
            ProvablyIdentical::default().decide(&a, &b),
            Decision::Drop(_)
        ));
    }

    #[test]
    fn phash_of_a_flat_image_is_stable() {
        assert_eq!(
            phash64(&vec![100; 64 * 64], 64, 64),
            phash64(&vec![100; 64 * 64], 64, 64)
        );
    }
}
