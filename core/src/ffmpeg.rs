//! Locating and invoking the bundled ffmpeg toolchain.
//!
//! Two rules from AGENTS.md are enforced here rather than left to callers:
//!
//!   * Arguments are always passed as an array. Nothing in this module builds a
//!     command line by concatenating strings, because `C:\Users\Nguyễn Văn A\`
//!     is a required test case, not an edge case.
//!   * The architecture of the binaries is checked against this build, and a
//!     mismatch is reported clearly instead of surfacing as a confusing
//!     "%1 is not a valid Win32 application" at first use.

use std::ffi::OsStr;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Machine field values from the PE header, for the architecture check.
const PE_MACHINE_AMD64: u16 = 0x8664;
const PE_MACHINE_ARM64: u16 = 0xAA64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arch {
    X64,
    Arm64,
}

impl Arch {
    /// The architecture this copy of `framekeep-core` was built for.
    pub fn current() -> Option<Self> {
        match std::env::consts::ARCH {
            "x86_64" => Some(Arch::X64),
            "aarch64" => Some(Arch::Arm64),
            _ => None,
        }
    }

    /// Directory name under `vendor/ffmpeg/`, matching upstream build naming.
    pub fn vendor_dir(self) -> &'static str {
        match self {
            Arch::X64 => "win64",
            Arch::Arm64 => "winarm64",
        }
    }

    fn from_pe_machine(machine: u16) -> Option<Self> {
        match machine {
            PE_MACHINE_AMD64 => Some(Arch::X64),
            PE_MACHINE_ARM64 => Some(Arch::Arm64),
            _ => None,
        }
    }
}

impl fmt::Display for Arch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Arch::X64 => "x64",
            Arch::Arm64 => "arm64",
        })
    }
}

#[derive(Debug)]
pub enum ToolchainError {
    NotFound {
        looked_in: Vec<PathBuf>,
    },
    ArchMismatch {
        binary: PathBuf,
        found: String,
        expected: Arch,
    },
}

impl fmt::Display for ToolchainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ToolchainError::NotFound { looked_in } => {
                writeln!(f, "Can't find the bundled ffmpeg. Framekeep won't process video without it.")?;
                writeln!(f, "Looked in:")?;
                for p in looked_in {
                    writeln!(f, "  {}", p.display())?;
                }
                write!(
                    f,
                    "Set FRAMEKEEP_FFMPEG_DIR to the folder holding ffmpeg and ffprobe, or reinstall the app."
                )
            }
            ToolchainError::ArchMismatch { binary, found, expected } => write!(
                f,
                "The bundled ffmpeg is built for {found}, but this is the {expected} build of Framekeep.\n\
                 Binary: {}\n\
                 Install the {expected} build, or point FRAMEKEEP_FFMPEG_DIR at matching binaries.",
                binary.display()
            ),
        }
    }
}

#[derive(Debug)]
pub struct Toolchain {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
    /// True when the binaries came off PATH rather than from the bundle, which
    /// means neither the version nor the licence build is under our control.
    pub from_path: bool,
}

impl Toolchain {
    /// Search order, most specific first. Every candidate that fails is kept so
    /// the error can say exactly where we looked.
    pub fn locate() -> Result<Self, ToolchainError> {
        let mut looked_in = Vec::new();
        let arch = Arch::current();

        if let Some(dir) = std::env::var_os("FRAMEKEEP_FFMPEG_DIR") {
            let dir = PathBuf::from(dir);
            if let Some(tc) = Self::try_dir(&dir, false) {
                return tc.verify_arch(arch);
            }
            looked_in.push(dir);
        }

        if let Ok(exe) = std::env::current_exe() {
            if let Some(exe_dir) = exe.parent() {
                // Shipped layout: binaries sit in an `ffmpeg` folder beside us.
                for candidate in [exe_dir.join("ffmpeg"), exe_dir.to_path_buf()] {
                    if let Some(tc) = Self::try_dir(&candidate, false) {
                        return tc.verify_arch(arch);
                    }
                    looked_in.push(candidate);
                }

                // Dev layout: walk up looking for vendor/ffmpeg/<arch>.
                if let Some(arch) = arch {
                    let mut dir: Option<&Path> = Some(exe_dir);
                    while let Some(d) = dir {
                        let candidate = d.join("vendor").join("ffmpeg").join(arch.vendor_dir());
                        if let Some(tc) = Self::try_dir(&candidate, false) {
                            return tc.verify_arch(Some(arch));
                        }
                        if candidate.parent().map(|p| p.exists()).unwrap_or(false) {
                            looked_in.push(candidate);
                        }
                        dir = d.parent();
                    }
                }
            }
        }

        // Last resort. Works, but it is not the build we tested or licensed.
        if let (Some(ffmpeg), Some(ffprobe)) = (which("ffmpeg"), which("ffprobe")) {
            return Toolchain {
                ffmpeg,
                ffprobe,
                from_path: true,
            }
            .verify_arch(arch);
        }

        Err(ToolchainError::NotFound { looked_in })
    }

    fn try_dir(dir: &Path, from_path: bool) -> Option<Self> {
        let ffmpeg = dir.join(exe_name("ffmpeg"));
        let ffprobe = dir.join(exe_name("ffprobe"));
        (ffmpeg.is_file() && ffprobe.is_file()).then_some(Toolchain {
            ffmpeg,
            ffprobe,
            from_path,
        })
    }

    /// Reads the PE header rather than trusting the folder name. A binary in a
    /// folder called `win64` is not evidence that it is an x64 binary.
    fn verify_arch(self, expected: Option<Arch>) -> Result<Self, ToolchainError> {
        let Some(expected) = expected else {
            return Ok(self);
        };
        match pe_machine(&self.ffmpeg) {
            Some(machine) => match Arch::from_pe_machine(machine) {
                Some(found) if found == expected => Ok(self),
                Some(found) => Err(ToolchainError::ArchMismatch {
                    binary: self.ffmpeg,
                    found: found.to_string(),
                    expected,
                }),
                None => Err(ToolchainError::ArchMismatch {
                    binary: self.ffmpeg,
                    found: format!("an unrecognised architecture (PE machine 0x{machine:04X})"),
                    expected,
                }),
            },
            // Not a readable PE file. On non-Windows this is normal, so it is
            // not treated as a failure -- the first real call will report it.
            None => Ok(self),
        }
    }

    /// Runs ffprobe. `args` stays a slice all the way down to the OS: no shell,
    /// no quoting, no concatenation.
    pub fn run_ffprobe<S: AsRef<OsStr>>(&self, args: &[S]) -> std::io::Result<Output> {
        let mut cmd = Command::new(&self.ffprobe);
        cmd.args(args);
        quiet(&mut cmd);
        cmd.output()
    }

    /// Runs ffmpeg. Same rule as above -- the path is one array element, so
    /// `C:\Users\Nguyễn Văn A\` never has to survive a quoting round-trip.
    pub fn run_ffmpeg<S: AsRef<OsStr>>(&self, args: &[S]) -> std::io::Result<Output> {
        let mut cmd = Command::new(&self.ffmpeg);
        cmd.args(args);
        quiet(&mut cmd);
        cmd.output()
    }

    /// What ffmpeg says it is, asked of the binary that will actually run.
    ///
    /// Not a constant anywhere in this repo, deliberately. The Settings screen
    /// shows this, and a hardcoded `7.1.1` there would keep reading `7.1.1`
    /// after the vendored build was replaced -- a number that describes an
    /// intention rather than the machine. `from_path` builds are exactly the
    /// case where the answer is not ours to predict.
    ///
    /// `None` when ffmpeg could not be run or said something unfamiliar. A
    /// missing version is not an error worth failing over: everything else
    /// about the toolchain is still true.
    pub fn version(&self) -> Option<Version> {
        let out = self.run_ffmpeg(&["-version"]).ok()?;
        Version::parse(&String::from_utf8_lossy(&out.stdout))
    }
}

/// The version string ffmpeg reports, plus the part of it a person reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    /// The whole build token, e.g. `7.1.1-full_build-www.gyan.dev`. Kept
    /// because it names the build, and the build is what a bug report needs.
    pub full: String,
    /// Just the numbers, e.g. `7.1.1`. What a settings line has room for.
    pub short: String,
}

impl Version {
    /// Reads the first line of `ffmpeg -version`, which every build opens with
    /// `ffmpeg version <token> Copyright …`.
    fn parse(stdout: &str) -> Option<Version> {
        let full = stdout
            .lines()
            .next()?
            .trim()
            .strip_prefix("ffmpeg version ")?
            .split_whitespace()
            .next()?
            .to_string();
        if full.is_empty() {
            return None;
        }
        // Distro builds prefix an `n` (`n7.1`), and every build may suffix a
        // vendor tail. The short form keeps digits and dots from the front and
        // stops at the first thing that is neither.
        let short: String = full
            .trim_start_matches('n')
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        let short = short.trim_end_matches('.').to_string();
        Some(Version {
            short: if short.is_empty() {
                full.clone()
            } else {
                short
            },
            full,
        })
    }
}

#[cfg(test)]
mod version_tests {
    use super::Version;

    #[test]
    fn reads_the_build_we_bundle() {
        // Verbatim first line from vendor/ffmpeg/win64.
        let v = Version::parse(
            "ffmpeg version 7.1.1-full_build-www.gyan.dev Copyright (c) 2000-2025 the FFmpeg developers\n\
             built with gcc 14.2.0 (Rev1, Built by MSYS2 project)\n",
        )
        .expect("the bundled build must parse");
        assert_eq!(v.full, "7.1.1-full_build-www.gyan.dev");
        assert_eq!(v.short, "7.1.1");
    }

    #[test]
    fn reads_the_builds_a_dev_machine_has_on_path() {
        // `from_path` is a real shipping-adjacent case: whatever the person
        // already had. Both shapes below are ffmpeg's own, from distro builds.
        let ubuntu = Version::parse("ffmpeg version 6.1.1-3ubuntu5 Copyright (c)\n").unwrap();
        assert_eq!(ubuntu.short, "6.1.1");
        let git = Version::parse("ffmpeg version n7.1 Copyright (c)\n").unwrap();
        assert_eq!(git.short, "7.1", "a leading n is a tag name, not a version");
        assert_eq!(git.full, "n7.1", "the full token stays as reported");
    }

    #[test]
    fn says_nothing_rather_than_guessing() {
        // A wrapper script, a localised build, an empty pipe: all reasons the
        // first line may not be ffmpeg's. None of them are worth inventing a
        // number for -- the Settings screen shows what it is given.
        assert_eq!(Version::parse(""), None);
        assert_eq!(Version::parse("bash: ffmpeg: command not found\n"), None);
        assert_eq!(Version::parse("ffmpeg version \n"), None);
    }

    #[test]
    fn an_unfamiliar_token_is_reported_whole() {
        // Some vendor builds lead with a name. Better to show it verbatim than
        // to show an empty box where a version was promised.
        let v = Version::parse("ffmpeg version SomeVendorBuild Copyright\n").unwrap();
        assert_eq!(v.short, "SomeVendorBuild");
        assert_eq!(v.full, "SomeVendorBuild");
    }
}

/// No console window for the child, ever.
///
/// A console-subsystem child of a *windowless* parent is given a brand-new
/// console by Windows. Run from a terminal nothing shows -- children attach to
/// the terminal you are already in -- which is why this was invisible through
/// all of S1/S2 and only surfaced when the tray, the first windowless parent,
/// started pasting: every ffmpeg and whisper spawn popped an empty Terminal on
/// the owner's desktop. Output still arrives through the pipes `.output()`
/// wires up; the flag only stops the window.
pub fn quiet(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = cmd;
}

fn exe_name(stem: &str) -> String {
    if cfg!(windows) {
        format!("{stem}.exe")
    } else {
        stem.to_string()
    }
}

fn which(stem: &str) -> Option<PathBuf> {
    let name = exe_name(stem);
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join(&name))
            .find(|p| p.is_file())
    })
}

/// Reads the Machine field out of a PE file: `e_lfanew` at 0x3C points at the
/// PE signature, and the field sits 4 bytes after it.
fn pe_machine(path: &Path) -> Option<u16> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;

    let mut mz = [0u8; 2];
    f.read_exact(&mut mz).ok()?;
    if &mz != b"MZ" {
        return None;
    }

    f.seek(SeekFrom::Start(0x3C)).ok()?;
    let mut off = [0u8; 4];
    f.read_exact(&mut off).ok()?;
    let pe_offset = u32::from_le_bytes(off) as u64;

    f.seek(SeekFrom::Start(pe_offset)).ok()?;
    let mut sig = [0u8; 4];
    f.read_exact(&mut sig).ok()?;
    if &sig != b"PE\0\0" {
        return None;
    }

    let mut machine = [0u8; 2];
    f.read_exact(&mut machine).ok()?;
    Some(u16::from_le_bytes(machine))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arch_maps_to_upstream_folder_names() {
        assert_eq!(Arch::X64.vendor_dir(), "win64");
        assert_eq!(Arch::Arm64.vendor_dir(), "winarm64");
    }

    #[test]
    fn pe_machine_reads_this_binary() {
        // The test binary itself is a PE file on Windows, so it is a free fixture.
        if cfg!(windows) {
            let exe = std::env::current_exe().unwrap();
            let machine = pe_machine(&exe).expect("test binary should be a readable PE file");
            assert_eq!(Arch::from_pe_machine(machine), Arch::current());
        }
    }

    #[test]
    fn pe_machine_rejects_non_pe_files() {
        let mut path = std::env::temp_dir();
        path.push("framekeep-not-a-pe.txt");
        std::fs::write(&path, b"this is not an executable").unwrap();
        assert_eq!(pe_machine(&path), None);
        let _ = std::fs::remove_file(&path);
    }
}
