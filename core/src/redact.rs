//! Painting over what must not be sent. S5.3.
//!
//! # The rectangles come from the caller
//!
//! This module does not decide what to hide. It could -- `scan` is right there
//! -- but then the review screen could not exist: a person who unticks a false
//! positive, or drags a box over something no pattern knows about (S5.6), is
//! changing the list, and a redactor that recomputes its own list would
//! overrule them both times. So the list is an argument.
//!
//! `--scan` fills the list from a scan for callers that have no person in the
//! loop. It is a convenience, not the design.
//!
//! # One box per word, never a bounding box
//!
//! `ocr::Reading::boxes_for` returns a rect per word for a reason, and it has
//! to survive to here. A GitHub token reads as two words -- the engine turns
//! `ghp_` into `ghp` and a space -- and the union of two rects that sit at
//! opposite ends of a line paints over everything between them. Covering
//! content nobody asked to hide is its own kind of broken, and on a screen
//! recording the thing in between is usually the very context the frame was
//! captured for.
//!
//! # Rounding goes outward, always
//!
//! Boxes arrive as floats and pixels are whole. Rounding the near edge down and
//! the far edge up covers at most a pixel too much; rounding the other way
//! leaves a sliver of a glyph showing, and a sliver of a glyph is a legible
//! character to anything that reads carefully.
//!
//! # Painting is not proof, and the proof has a ceiling
//!
//! The command reads the finished image back and scans it again, because boxes
//! in the right place and text actually gone are different claims.
//!
//! That check is weaker than it sounds, and this was measured rather than
//! guessed. Swapping `t=fill` for a two-pixel outline -- a redaction that leaves
//! the text plainly there for a person to read -- still came back
//! `still_detected: 0`, because four pixels off a fourteen-pixel line is enough
//! to stop *this* OCR engine while stopping nothing that reads better. The test
//! that caught that break was the dull one: a pixel in the middle of the box
//! must be black. Keep both. The arithmetic assertion is what pins the fill;
//! the round trip is what pins the coordinates.
//!
//! And read the round trip narrowly even so. **The verifier is the same scanner
//! that chose the boxes**, so it can only confirm that what was found is now
//! gone. It is blind to everything the scanner missed in the first place, and
//! at 16px that is about one secret in six.
//!
//! Measured on one frame while building this: five of six planted secrets were
//! covered, `still_detected` came back 0, and the email address sat in the
//! output in plain sight -- because the scan never found it, so nothing painted
//! over it and nothing looked for it afterwards. A zero here means "no
//! regression in the masking". It never means "this frame is safe to send".
//! The thing that means that is a person, which is why S5.4 exists.

use std::ffi::OsString;
use std::path::Path;

use crate::encode::{EncodeError, Format};
use crate::ffmpeg::Toolchain;
use crate::ocr;

/// A rectangle to paint out, in the frame's own whole pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mask {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Mask {
    /// Parses `x,y,w,h` and covers it outward to whole pixels.
    ///
    /// Fractions are accepted because callers pass OCR word boxes through
    /// verbatim, and those arrive as floats. Rounding them here -- through the
    /// same [`Mask::cover`] every other path uses -- is what keeps the outward
    /// rule in one place; the day a caller rounds for itself is the day one of
    /// the two copies starts rounding inward.
    pub fn parse(s: &str) -> Result<Mask, String> {
        let n: Vec<&str> = s.split(',').map(str::trim).collect();
        if n.len() != 4 {
            return Err(format!(
                "--box needs four numbers: x,y,w,h. Got {} value(s) in `{s}`.",
                n.len()
            ));
        }
        let mut v = [0f32; 4];
        for (i, part) in n.iter().enumerate() {
            v[i] = part
                .parse::<f32>()
                .ok()
                .filter(|x| x.is_finite() && *x >= 0.0)
                .ok_or_else(|| {
                    format!("--box values must be pixel numbers >= 0; `{part}` is not.")
                })?;
        }
        if v[2] <= 0.0 || v[3] <= 0.0 {
            return Err(format!(
                "--box must have a width and a height; got {}x{}. A zero-area box covers nothing.",
                v[2], v[3]
            ));
        }
        Ok(Mask::cover(ocr::Rect {
            x: v[0],
            y: v[1],
            w: v[2],
            h: v[3],
        }))
    }

    /// The whole pixels that fully contain a word box.
    ///
    /// Outward on every edge. Half a pixel of extra black costs nothing; half a
    /// pixel of remaining glyph is a readable character.
    pub fn cover(r: ocr::Rect) -> Mask {
        let x0 = r.x.floor().max(0.0);
        let y0 = r.y.floor().max(0.0);
        let x1 = (r.x + r.w).ceil().max(x0 + 1.0);
        let y1 = (r.y + r.h).ceil().max(y0 + 1.0);
        Mask {
            x: x0 as u32,
            y: y0 as u32,
            w: (x1 - x0) as u32,
            h: (y1 - y0) as u32,
        }
    }

    /// Trims a box to the frame.
    ///
    /// Clamped rather than refused, unlike `--region` on a crop, and the
    /// asymmetry is deliberate: a crop that is partly outside gives the caller a
    /// different picture than the one they asked about, while a mask that is
    /// partly outside still covers exactly what it can. Covering less than
    /// asked would be the dangerous direction, and clamping never does that.
    ///
    /// `None` when the box misses the frame entirely -- that is a caller who is
    /// working from the wrong coordinates, and painting nothing while reporting
    /// success is how a secret survives a redaction.
    pub fn clamp_to(self, width: u32, height: u32) -> Option<Mask> {
        if self.x >= width || self.y >= height {
            return None;
        }
        Some(Mask {
            x: self.x,
            y: self.y,
            w: self.w.min(width - self.x),
            h: self.h.min(height - self.y),
        })
    }

    fn to_filter(self) -> String {
        format!(
            "drawbox=x={}:y={}:w={}:h={}:color=black:t=fill",
            self.x, self.y, self.w, self.h
        )
    }
}

/// Reads a frame's pixel dimensions.
///
/// Needed before painting, so a box aimed outside the picture is reported
/// instead of quietly doing nothing: `drawbox` clips silently, and a mask that
/// clips to nothing looks exactly like a mask that worked.
pub fn dimensions(tools: &Toolchain, image: &Path) -> Result<(u32, u32), EncodeError> {
    let args: Vec<OsString> = vec![
        "-v".into(),
        "error".into(),
        "-select_streams".into(),
        "v:0".into(),
        "-show_entries".into(),
        "stream=width,height".into(),
        "-of".into(),
        "csv=p=0".into(),
        image.as_os_str().to_owned(),
    ];
    let out = tools.run_ffprobe(&args).map_err(EncodeError::Io)?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut parts = text.trim().split(',');
    let parsed = (|| {
        let w: u32 = parts.next()?.trim().parse().ok()?;
        let h: u32 = parts.next()?.trim().parse().ok()?;
        (w > 0 && h > 0).then_some((w, h))
    })();
    parsed.ok_or_else(|| EncodeError::Failed {
        path: image.display().to_string(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

/// Do the painted rectangles actually hold black pixels in the finished file?
///
/// This exists because the OCR round-trip turned out to be the wrong
/// instrument for the reviewed path -- learned from the first real approval.
/// A person had deliberately unticked two findings; the re-scan of the output
/// found them (as it must -- they were left visible on purpose), and a gate
/// reading `still_detected > 0` as "paint failed" parked a perfectly good
/// approval. A global re-scan cannot tell a regression from a decision.
///
/// This check asks only what the paint is answerable for: every pixel inside
/// every applied mask is black. Deterministic, per-box, needs no OCR engine --
/// so it also runs on the Linux job, which the OCR half never could.
///
/// Near-black rather than exactly zero: the pixel path through ffmpeg may
/// cross a YUV conversion, and limited-range roundtrips can land on 1 or 2.
/// Text that survived inside a box lands nowhere near 8.
pub fn boxes_are_black(
    tools: &Toolchain,
    image: &Path,
    masks: &[Mask],
) -> Result<bool, EncodeError> {
    let (width, height) = dimensions(tools, image)?;
    let args: Vec<OsString> = vec![
        "-hide_banner".into(),
        "-nostdin".into(),
        "-loglevel".into(),
        "error".into(),
        "-i".into(),
        image.as_os_str().to_owned(),
        "-f".into(),
        "rawvideo".into(),
        "-pix_fmt".into(),
        "rgb24".into(),
        "-".into(),
    ];
    let out = tools.run_ffmpeg(&args).map_err(EncodeError::Io)?;
    if !out.status.success() || out.stdout.len() != (width * height * 3) as usize {
        return Err(EncodeError::Failed {
            path: image.display().to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    Ok(masks.iter().all(|m| rect_is_black(&out.stdout, width, m)))
}

/// True when every pixel of `mask` in an rgb24 buffer is at most near-black.
fn rect_is_black(rgb: &[u8], width: u32, mask: &Mask) -> bool {
    for row in mask.y..mask.y + mask.h {
        let start = ((row * width + mask.x) * 3) as usize;
        let end = start + (mask.w * 3) as usize;
        if rgb[start..end].iter().any(|&channel| channel > 8) {
            return false;
        }
    }
    true
}

/// Paints every mask black and writes the result to `dest`.
///
/// The source is never modified. The review screen shows a person the original
/// frame with what was found on it, so overwriting in place would destroy the
/// only thing they have to check the redaction against.
pub fn apply(
    tools: &Toolchain,
    src: &Path,
    masks: &[Mask],
    dest: &Path,
    to: Format,
) -> Result<(), EncodeError> {
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir).map_err(EncodeError::Io)?;
    }

    let chain = masks
        .iter()
        .map(|m| m.to_filter())
        .collect::<Vec<_>>()
        .join(",");

    let mut args: Vec<OsString> = vec![
        "-hide_banner".into(),
        "-nostdin".into(),
        "-loglevel".into(),
        "error".into(),
        "-y".into(),
        "-i".into(),
        src.as_os_str().to_owned(),
    ];
    if !chain.is_empty() {
        args.push("-vf".into());
        args.push(chain.into());
    }
    if to == Format::Webp {
        args.extend([
            "-c:v".into(),
            "libwebp".into(),
            // Lossless here is not a preference. A lossy encoder smears the edge
            // of a black box, and smeared black over text is still text.
            "-lossless".into(),
            "1".into(),
            "-compression_level".into(),
            "6".into(),
        ]);
    }
    args.push(dest.as_os_str().to_owned());

    let out = tools.run_ffmpeg(&args).map_err(EncodeError::Io)?;
    if !out.status.success() {
        return Err(EncodeError::Failed {
            path: src.display().to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f32, y: f32, w: f32, h: f32) -> ocr::Rect {
        ocr::Rect { x, y, w, h }
    }

    #[test]
    fn a_word_box_is_covered_outward_never_inward() {
        // 10.7 .. 10.7+20.4 = 31.1  ->  10 .. 32
        let m = Mask::cover(rect(10.7, 20.2, 20.4, 9.9));
        assert_eq!(m.x, 10, "the near edge rounds down");
        assert_eq!(m.y, 20);
        assert_eq!(m.x + m.w, 32, "the far edge rounds up");
        assert_eq!(m.y + m.h, 31);
    }

    #[test]
    fn a_sub_pixel_box_still_covers_a_pixel() {
        // Nothing useful is this small, but a zero-height mask would be a
        // silently missing redaction rather than an error.
        let m = Mask::cover(rect(5.2, 5.2, 0.1, 0.1));
        assert!(m.w >= 1 && m.h >= 1);
    }

    #[test]
    fn a_box_hanging_over_the_edge_is_trimmed_not_dropped() {
        let m = Mask {
            x: 1900,
            y: 1070,
            w: 200,
            h: 200,
        }
        .clamp_to(1920, 1080)
        .expect("it overlaps the frame, so it must survive");
        assert_eq!((m.w, m.h), (20, 10));
    }

    #[test]
    fn a_box_entirely_outside_the_frame_is_refused_rather_than_ignored() {
        // drawbox would clip this to nothing and exit 0. Reporting success
        // while painting nothing is the failure this product cannot afford.
        assert!(Mask {
            x: 2000,
            y: 10,
            w: 50,
            h: 50
        }
        .clamp_to(1920, 1080)
        .is_none());
    }

    #[test]
    fn masks_become_one_filter_per_box_and_never_a_union() {
        let masks = [
            Mask {
                x: 10,
                y: 100,
                w: 30,
                h: 16,
            },
            Mask {
                x: 900,
                y: 100,
                w: 40,
                h: 16,
            },
        ];
        let chain = masks
            .iter()
            .map(|m| m.to_filter())
            .collect::<Vec<_>>()
            .join(",");
        assert_eq!(chain.matches("drawbox").count(), 2);
        // The gap between x=40 and x=900 belongs to the user. Nothing in this
        // chain may claim it.
        assert!(!chain.contains("w=930"), "that would be a bounding box");
        assert!(chain.contains("t=fill"), "an outline is not a redaction");
    }

    #[test]
    fn a_box_is_four_numbers_and_a_flat_one_is_refused() {
        assert_eq!(
            Mask::parse("10,20,30,40").unwrap(),
            Mask {
                x: 10,
                y: 20,
                w: 30,
                h: 40
            }
        );
        assert!(
            Mask::parse("10,20,0,40").is_err(),
            "zero width covers nothing"
        );
        assert!(Mask::parse("10,20,30").is_err(), "too few values");
        assert!(Mask::parse("10,20,30,x").is_err(), "not a number");
        assert!(Mask::parse("-5,20,30,40").is_err(), "negative coordinates");
    }

    #[test]
    fn a_black_rect_passes_and_one_lit_pixel_fails() {
        // A 4x3 rgb24 frame, all black except one bright pixel at (2,1).
        let mut rgb = vec![0u8; 4 * 3 * 3];
        rgb[(4 + 2) * 3] = 200; // row 1, column 2
        let all = Mask {
            x: 0,
            y: 0,
            w: 4,
            h: 3,
        };
        let clean = Mask {
            x: 0,
            y: 0,
            w: 2,
            h: 3,
        };
        assert!(!rect_is_black(&rgb, 4, &all), "the lit pixel must fail it");
        assert!(rect_is_black(&rgb, 4, &clean), "the clean half must pass");
    }

    #[test]
    fn near_black_from_a_yuv_roundtrip_still_counts_as_black() {
        // Limited-range YUV can bring pure black back as 1s and 2s. Text does
        // not survive at 8; a threshold that failed on 2 would flag every
        // correct paint that crossed a colorspace.
        let rgb = vec![2u8; 2 * 2 * 3];
        assert!(rect_is_black(
            &rgb,
            2,
            &Mask {
                x: 0,
                y: 0,
                w: 2,
                h: 2
            }
        ));
    }

    #[test]
    fn a_fractional_box_is_covered_outward_like_every_other_box() {
        // OCR word boxes arrive as floats and are passed through verbatim; the
        // outward rule must hold for them exactly as for scan-derived masks.
        // 10.5 .. 40.5 covers 10 .. 41.
        let m = Mask::parse("10.5,20.2,30,9.9").unwrap();
        assert_eq!((m.x, m.y), (10, 20), "near edges round down");
        assert_eq!(m.x + m.w, 41, "far edge rounds up");
        assert_eq!(m.y + m.h, 31);
    }
}
