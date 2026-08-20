//! Re-encoding kept frames, once the pipeline has decided which ones survive.
//!
//! # Why this exists at all
//!
//! Measured on a real 1920x1080 interface frame
//! (`docs/experiments/frame-byte-budget.md`):
//!
//! | | PNG | WebP lossless |
//! |---|---|---|
//! | 1920x1080 | 64.2 KB | **16.6 KB** |
//! | 1280x720 | 85.5 KB | 35.6 KB |
//!
//! WebP lossless is **3.9x smaller for the same pixels**, and scaling an
//! interface screenshot *down* makes it *bigger* -- interpolation invents
//! colours (653 become 1954) in the flat regions that were compressing well.
//!
//! # What this is NOT for
//!
//! An earlier version of this comment claimed WebP was the difference between
//! one frame per MCP reply and five. That was wrong, and measuring the cap
//! rather than assuming it is what showed why
//! (`docs/experiments/mcp-output-cap.md`): the reply cap counts **pixel area**,
//! not bytes. Ten megabytes of small images arrive intact while 266 KB of large
//! ones do not. PNG and WebP fit exactly the same nine full-HD frames.
//!
//! So this earns disk space in the cache and I/O when frames are read back --
//! worth having, much smaller than advertised. If a future client is measured
//! to count bytes, it becomes the lever there; none is known to today.
//!
//! # Why here and not in the MCP adapter
//!
//! `AGENTS.md`: video processing lives in `core/`, and an ffmpeg invocation in
//! `mcp/` is in the wrong place. The adapter asks for a format; this decides how
//! to produce it.
//!
//! # Why after dedup, not during extraction
//!
//! Dedup reads the written frames back and compares them, and it reads PNG.
//! Encoding first would mean either teaching dedup a second format or encoding
//! frames that are about to be thrown away. Doing it last costs nothing and
//! keeps both parts simple.

use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::ffmpeg::Toolchain;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Format {
    /// What the pipeline writes and what dedup reads.
    #[default]
    Png,
    /// Lossless. Not a quality trade -- `AGENTS.md` bans lossy encodings for
    /// screen content because artefacts make the model misread UI text, and
    /// lossless WebP does not lose a pixel.
    Webp,
}

impl Format {
    pub fn parse(s: &str) -> Option<Format> {
        match s {
            "png" => Some(Format::Png),
            "webp" => Some(Format::Webp),
            _ => None,
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Format::Png => "png",
            Format::Webp => "webp",
        }
    }
}

#[derive(Debug)]
pub enum EncodeError {
    Failed { path: String, stderr: String },
    Io(std::io::Error),
}

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EncodeError::Failed { path, stderr } => write!(
                f,
                "Couldn't re-encode a frame: {path}\nffmpeg said: {}\n\
                 The PNG frames are still there and still usable.",
                stderr
                    .lines()
                    .rev()
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or("(no output)")
            ),
            EncodeError::Io(e) => write!(f, "File error while re-encoding a frame: {e}"),
        }
    }
}

/// Converts one written frame, returning the new path and removing the old file.
///
/// The source is deleted only after the destination exists, so a failure leaves
/// the caller with the PNG rather than with nothing.
pub fn convert(tools: &Toolchain, png: &Path, to: Format) -> Result<PathBuf, EncodeError> {
    if to == Format::Png {
        return Ok(png.to_path_buf());
    }

    let dest = png.with_extension(to.extension());
    let args: Vec<OsString> = vec![
        "-hide_banner".into(),
        "-nostdin".into(),
        "-loglevel".into(),
        "error".into(),
        "-y".into(),
        "-i".into(),
        png.as_os_str().to_owned(),
        "-c:v".into(),
        "libwebp".into(),
        // Lossless, and told to spend effort: these frames are written once and
        // read by a model that is paying for every byte.
        "-lossless".into(),
        "1".into(),
        "-compression_level".into(),
        "6".into(),
        dest.clone().into_os_string(),
    ];

    let out = tools.run_ffmpeg(&args).map_err(EncodeError::Io)?;
    if !out.status.success() {
        return Err(EncodeError::Failed {
            path: png.display().to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }

    let _ = std::fs::remove_file(png);
    Ok(dest)
}

/// A rectangle of a frame, in the frame's own pixels.
///
/// # Why cropping is the budget lever
///
/// A client's reply cap counts **pixel area**, not bytes
/// (`docs/experiments/mcp-output-cap.md`): nine 1920x1080 frames fit, a tenth
/// starts cutting the reply. Cropping reduces area, so it buys room -- and
/// unlike scaling, it does not spend a single pixel of what is kept. Scaling
/// buys the same room by blurring the text this product exists to make
/// readable, which is why it is not offered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    pub x1: u32,
    pub y1: u32,
    pub x2: u32,
    pub y2: u32,
}

impl Region {
    /// Parses `x1,y1,x2,y2`. Rejects anything that would not name a rectangle,
    /// rather than silently clamping -- a caller who asked for the wrong area
    /// should be told, not handed a different one.
    pub fn parse(s: &str) -> Result<Region, String> {
        let n: Vec<&str> = s.split(',').map(str::trim).collect();
        if n.len() != 4 {
            return Err(format!(
                "--region needs four numbers: x1,y1,x2,y2. Got {} value(s) in `{s}`.",
                n.len()
            ));
        }
        let mut v = [0u32; 4];
        for (i, part) in n.iter().enumerate() {
            v[i] = part
                .parse()
                .map_err(|_| format!("--region values must be whole pixels; `{part}` is not."))?;
        }
        let r = Region {
            x1: v[0],
            y1: v[1],
            x2: v[2],
            y2: v[3],
        };
        if r.x2 <= r.x1 || r.y2 <= r.y1 {
            return Err(format!(
                "--region must have x2 > x1 and y2 > y1; got {},{} to {},{}.",
                r.x1, r.y1, r.x2, r.y2
            ));
        }
        Ok(r)
    }

    pub fn width(self) -> u32 {
        self.x2 - self.x1
    }
    pub fn height(self) -> u32 {
        self.y2 - self.y1
    }

    /// A folder name that is unique per region, so two different crops of one
    /// recording do not overwrite each other.
    pub fn dir_name(self) -> String {
        format!("crop-{}-{}-{}-{}", self.x1, self.y1, self.x2, self.y2)
    }
}

/// Crops one frame into `dest_dir`, keeping every pixel of what is inside the
/// rectangle at its original size.
pub fn crop(
    tools: &Toolchain,
    frame: &Path,
    region: Region,
    dest_dir: &Path,
    to: Format,
) -> Result<PathBuf, EncodeError> {
    std::fs::create_dir_all(dest_dir).map_err(EncodeError::Io)?;
    let name = frame
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "frame".into());
    let dest = dest_dir.join(format!("{name}.{}", to.extension()));

    let mut args: Vec<OsString> = vec![
        "-hide_banner".into(),
        "-nostdin".into(),
        "-loglevel".into(),
        "error".into(),
        "-y".into(),
        "-i".into(),
        frame.as_os_str().to_owned(),
        "-vf".into(),
        format!(
            "crop={}:{}:{}:{}",
            region.width(),
            region.height(),
            region.x1,
            region.y1
        )
        .into(),
    ];
    if to == Format::Webp {
        args.extend([
            "-c:v".into(),
            "libwebp".into(),
            "-lossless".into(),
            "1".into(),
            "-compression_level".into(),
            "6".into(),
        ]);
    }
    args.push(dest.clone().into_os_string());

    let out = tools.run_ffmpeg(&args).map_err(EncodeError::Io)?;
    if !out.status.success() {
        return Err(EncodeError::Failed {
            path: frame.display().to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_region_is_four_numbers_naming_a_rectangle() {
        let r = Region::parse("10,20,110,220").unwrap();
        assert_eq!((r.width(), r.height()), (100, 200));
        assert_eq!(r.dir_name(), "crop-10-20-110-220");
    }

    #[test]
    fn a_region_that_is_not_a_rectangle_is_refused_rather_than_fixed() {
        // Silently clamping would hand the caller a different area than the one
        // they asked about, and they would have no way to tell.
        assert!(Region::parse("100,10,10,200").is_err());
        assert!(Region::parse("10,10,10,200").is_err(), "zero width");
        assert!(Region::parse("1,2,3").is_err(), "too few values");
        assert!(Region::parse("1,2,3,x").is_err(), "not a number");
    }

    #[test]
    fn format_names_round_trip() {
        assert_eq!(Format::parse("png"), Some(Format::Png));
        assert_eq!(Format::parse("webp"), Some(Format::Webp));
        assert_eq!(Format::parse("jpeg"), None, "lossy formats are not offered");
        assert_eq!(Format::Webp.extension(), "webp");
    }

    #[test]
    fn png_conversion_is_a_no_op_rather_than_a_re_encode() {
        // Asking for the format the file already is should not spend an ffmpeg
        // process, and must not delete anything.
        let dir = std::env::temp_dir().join(format!("framekeep-encode-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("frame-000.png");
        std::fs::write(&p, b"not really a png").unwrap();

        let tools = crate::ffmpeg::Toolchain::locate();
        if let Ok(t) = tools {
            assert_eq!(convert(&t, &p, Format::Png).unwrap(), p);
        }
        assert!(p.exists(), "the source must survive a no-op conversion");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
