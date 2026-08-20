//! Locating whisper.cpp and picking a model that fits the machine.
//!
//! Same rules as the ffmpeg toolchain: array arguments, no shell, and a clear
//! message when something is missing rather than a failure at first use.

use std::ffi::OsStr;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A model we know how to recommend.
///
/// Sizes are **MiB**, matching what the registry reports and what Windows
/// Explorer shows. The plan writes the default as "574 MB" -- decimal
/// megabytes for the same file. One tool quoting two units for one download
/// reads as a bug, so everything here stays MiB.
// No `Eq`: realtime_factor is an f32, and a measured speed is not the kind of
// thing to compare for exact equality anyway.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Model {
    pub name: &'static str,
    pub file: &'static str,
    pub size_mib: u32,
    /// Rough working set while running, over and above the file itself.
    pub needs_ram_gb: u32,
    pub multilingual: bool,
    /// Seconds of audio handled per second of wall clock, measured on a 12-core
    /// x64 desktop using every thread. Roughly indicative, not a promise: the
    /// same box varied by ~45% between runs once it got warm.
    ///
    /// The spread here is the point. The default is *slower than realtime*,
    /// which is a very different product than one that transcribes a
    /// two-minute recording in nine seconds.
    pub realtime_factor: f32,
    /// False when the figure above is interpolated rather than timed. Shown to
    /// the user as `~5x` instead of `5x`, because a guess presented in the same
    /// column as a measurement is a guess wearing a measurement's clothes.
    pub speed_measured: bool,
}

impl Model {
    /// `14x` for a timed figure, `~5x` for an estimate.
    pub fn speed_label(&self) -> String {
        let prefix = if self.speed_measured { "" } else { "~" };
        format!("{prefix}{:.0}x", self.realtime_factor)
    }
}

/// Ordered best-first. `large-v3-turbo` quantised to q5_0 is the default the
/// plan settled on: multilingual, and small enough to ship.
pub const MODELS: &[Model] = &[
    Model {
        name: "large-v3-turbo-q5_0",
        file: "ggml-large-v3-turbo-q5_0.bin",
        size_mib: 547,
        needs_ram_gb: 8,
        multilingual: true,
        // Timed across two sessions: 1.17x and 1.5x on the same machine, the
        // spread being thermal. The low end is the one shown -- a transcription
        // that finishes sooner than promised is not the failure worth avoiding.
        realtime_factor: 1.2,
        speed_measured: true,
    },
    Model {
        name: "small",
        file: "ggml-small.bin",
        size_mib: 465,
        needs_ram_gb: 4,
        multilingual: true,
        // Timed 16/08/2026 on the 126s clip, two runs: 25.5s and 27.0s. The
        // interpolated guess this replaces was 5.0 -- it happened to be right.
        realtime_factor: 5.0,
        speed_measured: true,
    },
    Model {
        name: "base",
        file: "ggml-base.bin",
        size_mib: 141,
        needs_ram_gb: 2,
        multilingual: true,
        realtime_factor: 13.8,
        speed_measured: true,
    },
    Model {
        name: "tiny.en",
        file: "ggml-tiny.en.bin",
        size_mib: 74,
        needs_ram_gb: 1,
        multilingual: false,
        realtime_factor: 20.5,
        speed_measured: true,
    },
];

/// Best model this machine can comfortably run.
///
/// Deliberately conservative: a model that swaps turns a 30-second transcript
/// into a five-minute one, and the user has no way to tell why.
pub fn recommended_for(total_ram_gb: u32) -> &'static Model {
    MODELS
        .iter()
        .find(|m| total_ram_gb >= m.needs_ram_gb)
        .unwrap_or(&MODELS[MODELS.len() - 1])
}

#[cfg(windows)]
pub fn total_ram_gb() -> Option<u32> {
    #[repr(C)]
    struct MemoryStatusEx {
        length: u32,
        memory_load: u32,
        total_phys: u64,
        avail_phys: u64,
        total_page_file: u64,
        avail_page_file: u64,
        total_virtual: u64,
        avail_virtual: u64,
        avail_extended_virtual: u64,
    }
    extern "system" {
        fn GlobalMemoryStatusEx(buffer: *mut MemoryStatusEx) -> i32;
    }
    let mut status = MemoryStatusEx {
        length: std::mem::size_of::<MemoryStatusEx>() as u32,
        memory_load: 0,
        total_phys: 0,
        avail_phys: 0,
        total_page_file: 0,
        avail_page_file: 0,
        total_virtual: 0,
        avail_virtual: 0,
        avail_extended_virtual: 0,
    };
    // Safe: the struct is repr(C), `length` is set as the API requires, and the
    // pointer is to a live local.
    if unsafe { GlobalMemoryStatusEx(&mut status) } == 0 {
        return None;
    }
    Some((status.total_phys / 1_073_741_824) as u32)
}

#[cfg(not(windows))]
pub fn total_ram_gb() -> Option<u32> {
    None
}

#[derive(Debug)]
pub enum WhisperError {
    CliNotFound {
        looked_in: Vec<PathBuf>,
    },
    NoModel {
        dir: PathBuf,
        recommended: &'static Model,
    },
    /// `--model` named something that is neither a file nor a catalogue entry.
    ModelFileMissing(PathBuf),
}

impl fmt::Display for WhisperError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WhisperError::ModelFileMissing(given) => {
                write!(
                    f,
                    "No speech model called `{}`, and no file at that path.\n\
                     Run `framekeep-core models` to see the names, or point --model at a .bin file.",
                    given.display()
                )
            }
            WhisperError::CliNotFound { looked_in } => {
                writeln!(
                    f,
                    "Can't find whisper. Framekeep can extract frames without it, but not speech."
                )?;
                writeln!(f, "Looked in:")?;
                for p in looked_in {
                    writeln!(f, "  {}", p.display())?;
                }
                write!(f, "Set FRAMEKEEP_WHISPER_DIR to the folder holding whisper-cli, or reinstall the app.")
            }
            WhisperError::NoModel { dir, recommended } => write!(
                f,
                "No speech model installed, so there's nothing to transcribe with.\n\
                 Looked in: {}\n\
                 For this machine: {} ({} MiB).\n\
                 Download it, or pass --model with a path to one you already have.",
                dir.display(),
                recommended.name,
                recommended.size_mib
            ),
        }
    }
}

#[derive(Debug)]
pub struct Whisper {
    pub cli: PathBuf,
    pub model: PathBuf,
}

pub fn models_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("FRAMEKEEP_MODELS_DIR") {
        return Some(PathBuf::from(dir));
    }
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(|home| PathBuf::from(home).join(".framekeep").join("models"))
}

impl Whisper {
    pub fn locate(model_override: Option<&Path>) -> Result<Self, WhisperError> {
        let cli = locate_cli()?;
        let model = match model_override {
            Some(p) => resolve_model(p)?,
            None => find_model()?,
        };
        Ok(Whisper { cli, model })
    }

    /// Runs whisper-cli. Array arguments, straight to the OS, and no console
    /// window -- see `ffmpeg::quiet` for the failure this prevents.
    pub fn run<S: AsRef<OsStr>>(&self, args: &[S]) -> std::io::Result<Output> {
        let mut cmd = Command::new(&self.cli);
        cmd.args(args);
        crate::ffmpeg::quiet(&mut cmd);
        cmd.output()
    }
}

fn exe_name(stem: &str) -> String {
    if cfg!(windows) {
        format!("{stem}.exe")
    } else {
        stem.to_string()
    }
}

fn locate_cli() -> Result<PathBuf, WhisperError> {
    let name = exe_name("whisper-cli");
    let mut looked_in = Vec::new();

    if let Some(dir) = std::env::var_os("FRAMEKEEP_WHISPER_DIR") {
        let candidate = PathBuf::from(dir).join(&name);
        if candidate.is_file() {
            return Ok(candidate);
        }
        looked_in.push(candidate);
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            for candidate in [exe_dir.join("whisper").join(&name), exe_dir.join(&name)] {
                if candidate.is_file() {
                    return Ok(candidate);
                }
                looked_in.push(candidate);
            }
            // Dev layout: walk up to vendor/whisper/win64.
            let mut dir: Option<&Path> = Some(exe_dir);
            while let Some(d) = dir {
                let candidate = d.join("vendor").join("whisper").join("win64").join(&name);
                if candidate.is_file() {
                    return Ok(candidate);
                }
                dir = d.parent();
            }
        }
    }

    Err(WhisperError::CliNotFound { looked_in })
}

/// What `--model` was given: a catalogue name, or a path to a `.bin`.
///
/// Both, because both callers are real. A person at the terminal points at a
/// file they downloaded; the app stores a *name* from `models --json`, since a
/// path saved in a settings file goes stale the moment the models folder moves
/// -- which is exactly the failure `frames.json` had.
///
/// A name that is not in the catalogue and not a file on disk is an error
/// rather than a silent fall back to the default: a typo that quietly
/// transcribes with a different model than the one asked for is a result
/// nobody can explain afterwards.
fn resolve_model(given: &Path) -> Result<PathBuf, WhisperError> {
    if given.is_file() {
        return Ok(given.to_path_buf());
    }
    if let Some(name) = given.to_str() {
        if let Some(m) = MODELS.iter().find(|m| m.name == name) {
            let dir = models_dir().unwrap_or_else(|| PathBuf::from("models"));
            let path = dir.join(m.file);
            if path.is_file() {
                return Ok(path);
            }
            return Err(WhisperError::NoModel {
                dir,
                recommended: m,
            });
        }
    }
    Err(WhisperError::ModelFileMissing(given.to_path_buf()))
}

/// Picks the best installed model, preferring the order in [`MODELS`].
fn find_model() -> Result<PathBuf, WhisperError> {
    let dir = models_dir().unwrap_or_else(|| PathBuf::from("models"));
    for m in MODELS {
        let candidate = dir.join(m.file);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    // Development convenience: a `models/` folder beside the repo.
    if let Ok(exe) = std::env::current_exe() {
        let mut d: Option<&Path> = exe.parent();
        while let Some(cur) = d {
            for m in MODELS {
                let candidate = cur.join("models").join(m.file);
                if candidate.is_file() {
                    return Ok(candidate);
                }
            }
            d = cur.parent();
        }
    }
    let recommended = recommended_for(total_ram_gb().unwrap_or(8));
    Err(WhisperError::NoModel { dir, recommended })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_small_machine_is_not_offered_the_big_model() {
        // A model that swaps turns a 30-second transcript into a five-minute
        // one, and the user cannot tell why.
        assert_eq!(recommended_for(2).name, "base");
        assert_eq!(recommended_for(1).name, "tiny.en");
    }

    #[test]
    fn a_normal_machine_gets_the_multilingual_default() {
        let m = recommended_for(16);
        assert_eq!(m.name, "large-v3-turbo-q5_0");
        assert!(
            m.multilingual,
            "the default has to handle more than English"
        );
    }

    #[test]
    fn recommendation_never_panics_on_absurd_input() {
        assert_eq!(recommended_for(0).name, "tiny.en");
        assert_eq!(recommended_for(u32::MAX).name, "large-v3-turbo-q5_0");
    }

    #[test]
    fn every_model_entry_is_self_consistent() {
        for m in MODELS {
            assert!(
                m.file.starts_with("ggml-"),
                "{} has an unexpected filename",
                m.name
            );
            assert!(
                m.size_mib > 0 && m.needs_ram_gb > 0,
                "{} has a zero budget",
                m.name
            );
        }
        // Ordered best-first, so `find` picks the largest that fits.
        for pair in MODELS.windows(2) {
            assert!(
                pair[0].needs_ram_gb >= pair[1].needs_ram_gb,
                "MODELS must stay ordered by requirement, or the recommendation inverts"
            );
        }
    }
}
