//! `~/.framekeep/settings.json` -- what the user has decided.
//!
//! Promised by the retention spec and missing until now, which left a hole:
//! `choice_made` was hardcoded `false`, so the one-time question in L5b could
//! never be answered and auto-delete could never actually run. A setting with
//! nowhere to live is a setting that does not exist.
//!
//! # The rules this file follows
//!
//! **No file means defaults, and the defaults do nothing.** Watching is off,
//! nothing has been agreed to. A fresh install watches no folders and deletes
//! no recordings, and it gets there by having no opinions rather than by
//! writing a file full of them.
//!
//! **A file we cannot read is left alone.** Same rule the MCP adapter's `init`
//! follows for client configs: report it, use defaults for this run, and do
//! not overwrite. Someone hand-edited it and a stray comma should not cost
//! them their settings.
//!
//! **Writes are atomic.** `.partial` then rename, the same discipline as the
//! model download and the transcript store -- a half-written settings file on
//! a power cut is a broken app on next start.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub retention: RetentionSettings,
    /// `None` is off, and off is the default -- derived, so the default can
    /// never drift into meaning something. Folder watching is a secondary
    /// entry point; the clipboard is the main one, so this starts silent.
    pub watch: Option<Watch>,
    /// How the window looks. `System` follows the OS, which is also what the
    /// CSS does when the root carries no `data-theme` -- absent-means-default,
    /// the same shape as `watch`.
    pub theme: Theme,
    pub redaction: RedactionSettings,
    pub app: AppSettings,
    pub transcription: TranscriptionSettings,
}

/// How speech becomes words. Every field is `None` by default, and `None`
/// means "let core decide" rather than a value chosen here -- core picks the
/// model that fits the machine's RAM and lets whisper detect the language,
/// and both of those are better answers than a default frozen in this file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TranscriptionSettings {
    /// A name from core's own catalogue (`framekeep-core models --json`), not
    /// a path. The catalogue lives in core because core owns the files; a
    /// second list here would drift the day one of them gained an entry.
    pub model: Option<String>,
    /// A whisper language code, or `None` to let it detect. Detection is right
    /// far more often than a guess, so this is for when it is wrong.
    pub language: Option<String>,
    /// Where models are downloaded. `None` is core's default,
    /// `~/.framekeep/models`. **Must come from a folder picker** -- same
    /// packaging constraint as `Watch`, for the same reason.
    pub models_dir: Option<PathBuf>,
}

/// How the app behaves around the window and the session.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    /// Closing the window hides it and the app keeps running in the tray.
    ///
    /// On by default because the IPC server is the product: an app that quits
    /// when its window closes takes the adapter's bridge down mid-conversation,
    /// and a model that was mid-answer gets a connection error instead of
    /// frames. Turning it off makes the close button mean close.
    pub minimize_to_tray: bool,
    /// Whether the packaged app is registered to start with Windows.
    ///
    /// Mirrors the system state rather than driving it: Windows owns the real
    /// answer (a user can disable a startup task in Task Manager and never
    /// tell us), so this is refreshed from the system on read.
    pub start_on_login: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        AppSettings {
            minimize_to_tray: true,
            start_on_login: false,
        }
    }
}

/// What the scanner looks for beyond its built-in rules. S5.8.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RedactionSettings {
    /// Words a person added, in the order they added them. Empty by default,
    /// and empty is not a lesser state: the built-in rules are what the 83%
    /// was measured on, and these only ever add to them.
    ///
    /// Stored as typed, not as anything derived. What survives OCR damage is
    /// core's business (`secrets::fold`), and freezing a normalised form here
    /// would mean a scanner improvement could not reach patterns already saved.
    pub patterns: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RetentionSettings {
    /// `Auto-delete completed recordings` in Settings.
    pub delete_copied_sources: bool,
    /// Whether the one-time question has been answered. Until it is, nothing
    /// Framekeep created is deleted, whatever the toggle says (L5b).
    pub choice_made: bool,
    pub keep_days: u64,
}

impl Default for RetentionSettings {
    fn default() -> Self {
        RetentionSettings {
            // On, and safe to be on precisely because `choice_made` gates it.
            delete_copied_sources: true,
            choice_made: false,
            keep_days: crate::retention::DEFAULT_KEEP_DAYS,
        }
    }
}

/// One watched folder. One, not a list: a product whose main entry is the
/// clipboard does not need a folder tree, and every extra folder is another
/// place to be surprised by.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Watch {
    /// **Must come from a folder picker**, not from a typed path.
    ///
    /// This is a packaging constraint, not a preference. Reading an arbitrary
    /// path from a packaged (MSIX) app needs `broadFileSystemAccess`, which is
    /// a *restricted* capability: it has to be justified at Store submission
    /// and a refusal fails certification. A folder the user picked is granted
    /// to the app by the system without it. The watcher takes this path and
    /// asks no questions; where it came from is the caller's promise to keep.
    pub folder: PathBuf,
    /// Only recordings modified after this are imported. Set when watching is
    /// switched on, so pointing at a folder of two hundred old videos imports
    /// none of them -- and so a recording made while the app was closed still
    /// arrives on the next start.
    pub since: i64,
}

/// Whether a word may be saved as a custom pattern, and if not, the sentence
/// a person reads.
///
/// This is core's `secrets::check_pattern`, mirrored, and the mirroring is
/// deliberate: reaching the real one means depending on the crate that carries
/// ffmpeg, whisper and WinRT, to borrow six lines. What makes the duplicate
/// safe is *where* it sits. Core refuses a bad `--pattern` with a usage error,
/// and the scan is one step of `run_stages` -- a word core would reject, saved
/// here, turns every later import red. So this side has to be the strict one,
/// and if the two ever drift the cost is a word Settings will not take, which
/// somebody can see, rather than a pipeline that dies, which they cannot.
///
/// The length is counted after discounting separators, because those are what
/// OCR eats and they cover nothing on screen.
pub fn check_pattern(word: &str) -> Result<String, String> {
    // Three characters is `the`. A pattern that fires on every word starting
    // with `the` does not hide a secret, it hides the screen -- and a review
    // list nobody can read costs the 20% only a person catches.
    const MIN_PATTERN: usize = 4;

    let word = word.trim();
    if word.is_empty() {
        return Err("Type a word or phrase to hide.".into());
    }
    if word.chars().filter(|c| !" _-.".contains(*c)).count() < MIN_PATTERN {
        return Err(format!(
            "Use at least {MIN_PATTERN} letters or digits. Shorter patterns match ordinary \
             words and bury the real findings."
        ));
    }
    Ok(word.to_string())
}

/// How many a person may keep. Core enforces the same cap on `--pattern`.
pub const MAX_PATTERNS: usize = 20;

pub fn path() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(|home| PathBuf::from(home).join(".framekeep").join("settings.json"))
}

/// Read the settings, plus anything worth telling the user about how it went.
///
/// Never fails: there is always a usable answer, because an app that will not
/// start over a malformed preferences file is worse than one that starts with
/// defaults and says so.
pub fn load() -> (Settings, Option<String>) {
    let Some(file) = path() else {
        return (Settings::default(), None);
    };
    load_from(&file)
}

pub fn load_from(file: &Path) -> (Settings, Option<String>) {
    let text = match std::fs::read_to_string(file) {
        Ok(t) => t,
        // Absent is the normal case on a fresh install, and it is not news.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Settings::default(), None),
        Err(e) => {
            return (
                Settings::default(),
                Some(format!(
                    "Couldn't read your settings ({e}). Using defaults for now; the file was left alone."
                )),
            )
        }
    };

    match serde_json::from_str(&text) {
        Ok(settings) => (settings, None),
        Err(e) => (
            Settings::default(),
            Some(format!(
                "Your settings file has something Framekeep can't read (line {}). \
                 Using defaults for now -- the file was left alone, so nothing you set is lost.",
                e.line()
            )),
        ),
    }
}

pub fn save(settings: &Settings) -> std::io::Result<()> {
    let Some(file) = path() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no home folder to save settings in",
        ));
    };
    save_to(&file, settings)
}

pub fn save_to(file: &Path, settings: &Settings) -> std::io::Result<()> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Through a neighbour, then rename: the real path never holds half a file.
    let partial = file.with_extension("partial");
    let text = serde_json::to_string_pretty(settings).map_err(std::io::Error::other)?;
    std::fs::write(&partial, text)?;
    match std::fs::rename(&partial, file) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&partial);
            Err(e)
        }
    }
}

impl Settings {
    /// The retention rules these settings describe.
    pub fn retention(&self) -> crate::retention::Retention {
        crate::retention::Retention {
            keep_for: std::time::Duration::from_secs(self.retention.keep_days * 24 * 60 * 60),
            delete_copied_sources: self.retention.delete_copied_sources,
            choice_made: self.retention.choice_made,
            recordings_dir: crate::retention::default_recordings_dir().unwrap_or_default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static N: AtomicU32 = AtomicU32::new(0);

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "framekeep-settings-{}-{}-{name}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("settings.json")
    }

    /// A fresh install must do nothing it was not asked to do.
    #[test]
    fn no_file_means_watching_nothing_and_deleting_nothing() {
        let (settings, complaint) = load_from(&temp("absent"));
        assert!(settings.watch.is_none(), "a fresh install watched a folder");
        assert!(
            !settings.retention.choice_made,
            "consent appeared from nowhere"
        );
        assert!(complaint.is_none(), "an absent file is normal, not news");

        // And the rules derived from it refuse to delete anything of the user's.
        let retention = settings.retention();
        assert!(!retention.choice_made);
    }

    #[test]
    fn what_was_saved_is_what_comes_back() {
        let file = temp("roundtrip");
        let mut settings = Settings::default();
        settings.retention.choice_made = true;
        settings.watch = Some(Watch {
            folder: PathBuf::from(r"C:\Users\Nguyễn Văn A\Videos"),
            since: 1_700_000_000,
        });
        save_to(&file, &settings).unwrap();

        let (back, complaint) = load_from(&file);
        assert!(complaint.is_none());
        assert!(back.retention.choice_made);
        let watch = back.watch.expect("the watched folder was lost");
        assert_eq!(watch.folder, PathBuf::from(r"C:\Users\Nguyễn Văn A\Videos"));
        assert_eq!(watch.since, 1_700_000_000);
    }

    /// Hand-edited into invalid JSON. The file is theirs; we do not get to
    /// replace it because we could not parse it.
    #[test]
    fn a_broken_file_is_reported_and_left_exactly_as_it_was() {
        let file = temp("broken");
        let original = "{ \"retention\": { \"choice_made\": true,, } }";
        std::fs::write(&file, original).unwrap();

        let (settings, complaint) = load_from(&file);
        assert!(
            !settings.retention.choice_made,
            "defaults must not inherit a broken file"
        );
        let said = complaint.expect("a broken settings file has to be reported");
        assert!(said.contains("nothing you set is lost"), "{said}");

        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            original,
            "Framekeep overwrote a file it could not read"
        );
    }

    #[test]
    fn the_theme_round_trips_and_defaults_to_system() {
        let file = temp("theme");
        let (fresh, _) = load_from(&file);
        assert_eq!(fresh.theme, Theme::System, "a fresh install follows the OS");

        let settings = Settings {
            theme: Theme::Dark,
            ..Default::default()
        };
        save_to(&file, &settings).unwrap();
        let (back, _) = load_from(&file);
        assert_eq!(back.theme, Theme::Dark);
        // And on the wire it is the lowercase word the CSS hook expects.
        assert!(serde_json::to_string(&back).unwrap().contains("\"dark\""));
    }

    /// New keys arrive in later versions; an older file must still load.
    #[test]
    fn a_file_from_an_older_version_still_loads() {
        let file = temp("partial");
        std::fs::write(&file, r#"{"retention":{"choice_made":true}}"#).unwrap();
        let (settings, complaint) = load_from(&file);
        assert!(complaint.is_none());
        assert!(settings.retention.choice_made);
        // Missing keys fall back to the defaults, not to zero.
        assert_eq!(
            settings.retention.keep_days,
            crate::retention::DEFAULT_KEEP_DAYS
        );
        assert!(settings.retention.delete_copied_sources);
    }

    /// The same table core's own test uses, so a drift between the two shows
    /// up as a failure on this side rather than as a red recording on
    /// somebody's machine.
    #[test]
    fn a_pattern_too_short_to_be_safe_is_refused_before_it_can_be_saved() {
        for bad in ["", "   ", "ab", "a_b", "a-b.c"] {
            assert!(
                check_pattern(bad).is_err(),
                "{bad:?} was accepted; core would refuse it and the scan would die"
            );
        }
        assert_eq!(check_pattern("  acme  ").unwrap(), "acme");
        assert_eq!(check_pattern("MÃ NHÂN VIÊN").unwrap(), "MÃ NHÂN VIÊN");
        // Separators are discounted, so this is four letters and passes.
        assert_eq!(check_pattern("a_c_m_e").unwrap(), "a_c_m_e");
    }

    /// What a person typed is stored as typed. Normalising here would freeze
    /// today's idea of OCR damage into their settings file, where a later
    /// scanner improvement could never reach it.
    #[test]
    fn patterns_round_trip_exactly_as_they_were_typed() {
        let file = temp("patterns");
        let mut settings = Settings::default();
        assert!(
            settings.redaction.patterns.is_empty(),
            "a fresh install invented a pattern"
        );
        settings.redaction.patterns = vec!["Project Nightingale".into(), "MÃ NHÂN VIÊN".into()];
        save_to(&file, &settings).unwrap();

        let (back, complaint) = load_from(&file);
        assert!(complaint.is_none());
        assert_eq!(back.redaction.patterns, settings.redaction.patterns);
    }

    #[test]
    fn a_save_leaves_no_partial_behind() {
        let file = temp("atomic");
        save_to(&file, &Settings::default()).unwrap();
        assert!(file.exists());
        assert!(
            !file.with_extension("partial").exists(),
            "a .partial survived a successful save"
        );
    }
}
