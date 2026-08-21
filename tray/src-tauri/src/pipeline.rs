//! What happens to a recording after it is pasted. The S4 stage machine,
//! grown its S5 stage.
//!
//! ```text
//! Extracting frames ──> Transcribing ──> Scanning ──> Needs review ──> Ready
//!         │                  │              │             (findings)  (clean)
//!         └──────────────────┴──────────────┴───────> Error (with a sentence)
//! ```
//!
//! "Scanning for secrets" was deliberately absent until S5.1 landed the
//! scanner -- showing the chip earlier would have been a claim that we looked
//! when we did not. It is real now, and the same honesty cuts the other way:
//! `finding_count` stays NULL when no engine could look, because "nobody
//! looked" and "looked and found nothing" must never share a value.
//!
//! One stage remains deliberately absent: there is no "Processing". Whisper
//! runs for minutes, and a person watching the queue deserves to know which
//! half of the work is happening.
//!
//! Rows leave `needs_review` in exactly one place, and it is not here -- see
//! `review::apply`. The pipeline only ever walks forward.
//!
//! All of this is a shell around `framekeep-core`. Not one ffmpeg argument
//! appears in this file, and none may: the tray knows *that* a video becomes
//! frames and words, never *how*.
//!
//! The [`Runner`] trait exists so every transition is testable on a machine
//! with no core binary, no ffmpeg and no video files -- the same reasoning as
//! `Session::handle_line`. The real runner spawns the binary with args as an
//! array, which is the house rule (`C:\Users\Nguyễn Văn A` must survive).

use crate::queue::{Queue, Recording, Status};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Runs one core command, returns its stdout. The error is the human-facing
/// sentence core wrote to stderr -- core keeps stdout machine-clean.
pub trait Runner: Send + Sync {
    fn run(&self, args: &[String]) -> Result<String, String>;

    /// Where models should be downloaded, if the person chose somewhere.
    ///
    /// An environment variable rather than a flag because that is how core
    /// already takes it -- the same `FRAMEKEEP_MODELS_DIR` a developer sets by
    /// hand. Default: core's own answer, which is `~/.framekeep/models`.
    fn models_dir(&self) -> Option<PathBuf> {
        None
    }
}

/// What `core map --skip-transcript` answers, in the fields the queue keeps.
///
/// One call, not probe-then-frames: `map` is core's own orchestration of the
/// two, it reuses cached frames on a re-run, and it is the only command that
/// writes `frames.json` -- which is where the queue screen's thumbnails come
/// from. The first version spawned probe and frames separately and got
/// everything except the index; the missing thumbnails on screen were the
/// symptom.
#[derive(Debug, serde::Deserialize)]
struct MapReport {
    video: VideoInfo,
    frames: Vec<FrameOut>,
    /// Only on the `--scan` call. Kept as a raw value because it is saved to
    /// `scan.json` verbatim -- evidence first, and the typed view of it lives
    /// in `review.rs`, which is the module that acts on it.
    #[serde(default)]
    scan: Option<serde_json::Value>,
}

#[derive(Debug, serde::Deserialize)]
struct VideoInfo {
    duration_seconds: Option<f64>,
    width: u32,
    height: u32,
    has_audio: bool,
}

#[derive(Debug, serde::Deserialize)]
struct FrameOut {
    #[allow(dead_code)]
    pts_time: f64,
    #[allow(dead_code)]
    file: String,
}

/// Process one recording, updating its row as each stage completes.
///
/// `notify` fires after every visible change, so a window can repaint. It
/// carries the handle and nothing else -- whoever listens re-reads the queue,
/// which keeps this file free of any opinion about what a UI wants.
///
/// `patterns` are the words a person added in Settings (S5.8). They are passed
/// in rather than read here for the same reason core takes them as a flag: a
/// background thread that reaches for the settings file gives tests a different
/// answer depending on whose machine they run on.
pub fn process(
    queue: &Queue,
    handle: &str,
    runner: &dyn Runner,
    patterns: &[String],
    speech: &crate::settings::TranscriptionSettings,
    notify: &dyn Fn(&str),
) {
    let Ok(Some(recording)) = queue.get(handle) else {
        return; // Removed while waiting its turn. Nothing to do, nothing to say.
    };

    match run_stages(queue, &recording, runner, patterns, speech, notify) {
        Ok(()) => {}
        Err(message) => {
            // Re-read rather than reuse the pre-stage snapshot: the map stage
            // already saved duration and frame count, and writing the snapshot
            // back erased them -- the clean-machine run's error rows showed
            // "--" for a recording whose frames had extracted fine, which sent
            // the diagnosis hunting a map failure that never happened.
            let mut row = queue.get(handle).ok().flatten().unwrap_or(recording);
            row.status = Status::Error;
            row.error = Some(message);
            let _ = queue.put(&row);
            notify(handle);
        }
    }
}

fn run_stages(
    queue: &Queue,
    recording: &Recording,
    runner: &dyn Runner,
    patterns: &[String],
    speech: &crate::settings::TranscriptionSettings,
    notify: &dyn Fn(&str),
) -> Result<(), String> {
    let path = recording.source_path.to_string_lossy().into_owned();
    let mut row = recording.clone();

    // -- frames and video info in one call: the fast half, ~2s for SDR -------
    // webp because it is lossless and 3.9x smaller on interface content; the
    // format choice itself lives in core, this only asks.
    let report: MapReport = json(runner.run(&[
        "map".into(),
        path.clone(),
        "--skip-transcript".into(),
        "--format".into(),
        "webp".into(),
    ])?)?;
    row.duration_ms = report.video.duration_seconds.map(|s| (s * 1000.0) as i64);
    row.width = Some(report.video.width as i64);
    row.height = Some(report.video.height as i64);
    row.frame_count = Some(report.frames.len() as i64);
    queue.put(&row).map_err(|e| e.to_string())?;
    notify(&row.handle);

    // -- transcript: the slow half, minutes at the default model's ~1x -------
    //
    // Gated on the machine actually being able to transcribe. The first paste
    // on a clean install is a video with sound and no model downloaded yet,
    // and until 21/08 that combination turned the whole recording into an
    // error row -- over equipment the product's own Settings copy calls
    // optional ("Frames still work. Recordings arrive without a transcript.").
    // The clean-machine run for the Store found it on its first recording.
    //
    // Asked of doctor rather than recovered from transcribe's failure,
    // because the failure is core's human-facing sentence, and matching on
    // prose is the same trap as the checker that greps comments. A transcribe
    // failure with a model present still errors the row -- that stays a real
    // fault, and the test for it stays red-provable.
    if report.video.has_audio && speech_available(runner) {
        row.status = Status::Transcribing;
        queue.put(&row).map_err(|e| e.to_string())?;
        notify(&row.handle);
        // A cached transcript returns in milliseconds; core handles that.
        let mut args: Vec<String> = vec!["transcribe".into(), path.clone()];
        if let Some(model) = &speech.model {
            args.push("--model".into());
            args.push(model.clone());
        }
        if let Some(code) = &speech.language {
            args.push("--language".into());
            args.push(code.clone());
        }
        runner.run(&args)?;
    }

    // -- scan: fast, because the frames are already on disk ------------------
    // A second `map` call rather than a separate command: core serves the
    // frames from its own store (measured at ~1s of extraction saved) and the
    // scan itself is ~56ms a frame. `--scan` is explicit here and must never
    // become the default -- see core's usage text for why.
    row.status = Status::Scanning;
    queue.put(&row).map_err(|e| e.to_string())?;
    notify(&row.handle);
    let mut scan_args: Vec<String> = vec![
        "map".into(),
        path.clone(),
        "--skip-transcript".into(),
        "--scan".into(),
        "--format".into(),
        "webp".into(),
    ];
    for word in patterns {
        // One flag per word, as an array element -- never joined into a string.
        // These are typed by a person and contain spaces and accents by design.
        scan_args.push("--pattern".into());
        scan_args.push(word.clone());
    }
    let scanned: MapReport = json(runner.run(&scan_args)?)?;

    // Three states, kept apart on purpose:
    //   scan ran, found things   -> needs_review, count > 0
    //   scan ran, found nothing  -> ready, count = 0
    //   no engine to run         -> ready, count stays NULL
    // The window renders NULL and 0 the same ("—" vs nothing to show), but the
    // database keeps them distinct: "nobody looked" must never be promoted to
    // "looked and found nothing" by a display convenience.
    let findings = match &scanned.scan {
        Some(scan) if scan["engine"]["available"].as_bool() == Some(true) => {
            crate::review::save_scan(&queue.cache_dir(&row.handle), scan)?;
            Some(scan["detections_total"].as_u64().unwrap_or(0) as i64)
        }
        _ => None,
    };

    row.finding_count = findings;
    row.status = match findings {
        Some(n) if n > 0 => Status::NeedsReview,
        _ => Status::Ready,
    };
    queue.put(&row).map_err(|e| e.to_string())?;
    notify(&row.handle);
    Ok(())
}

fn json<T: serde::de::DeserializeOwned>(stdout: String) -> Result<T, String> {
    serde_json::from_str(&stdout).map_err(|e| {
        format!("framekeep-core answered something unreadable ({e}). Report this with the video's format.")
    })
}

/// Whether this machine can transcribe at all -- the same answer the Settings
/// screen trusts, from `doctor --json`.
///
/// Anything short of a confident yes counts as no: a doctor that cannot be
/// run, or answers something unreadable, must degrade the recording to
/// frames-only rather than sink it. Frames are the product; speech is a bonus.
fn speech_available(runner: &dyn Runner) -> bool {
    runner
        .run(&["doctor".into(), "--json".into()])
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .map(|d| d["whisper"]["available"].as_bool() == Some(true))
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------

/// The real thing: spawns `framekeep-core` with an argument array.
pub struct CoreBinary {
    bin: PathBuf,
    models_dir: Option<PathBuf>,
}

/// What core's binary is called here. One definition, because the tests need
/// it too and a second `cfg!` in the test module is a second thing to get
/// wrong -- the first version hard-coded `.exe` and the Linux job failed it.
fn core_name() -> &'static str {
    if cfg!(windows) {
        "framekeep-core.exe"
    } else {
        "framekeep-core"
    }
}

/// Every place core might be, in the order they are tried.
///
/// Split out from `locate` so it can be tested: the real one reads
/// `current_exe`, which in a test is the test harness, so the interesting
/// case -- an installed app -- is unreachable without passing the path in.
///
/// The shipped branch was missing until S6.1 and a packaged build would have
/// failed on it. `ffmpeg` and `whisper` both look beside the executable first;
/// this one knew only the dev tree, so an installed app would have reported
/// "core not found" and listed source paths that do not exist on that machine.
fn core_candidates(exe: Option<&Path>, env: Option<PathBuf>) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(env) = env {
        candidates.push(env);
    }
    let Some(exe) = exe else {
        return candidates;
    };
    let name = core_name();

    // Beside us: what an installed app looks like, MSIX or otherwise. Second
    // rather than first, and safe there because in the dev tree core is *not*
    // beside the tray binary -- so this can never shadow the branch below.
    if let Some(dir) = exe.parent() {
        candidates.push(dir.join(name));
    }

    // tray/src-tauri/target/{debug,release}/x.exe -> repo root.
    // ancestors() yields the path itself first, so the repo root is five steps
    // up, not four -- the first version said four, and the error message is
    // what caught it: it printed the wrong paths it had looked at.
    if let Some(repo) = exe.ancestors().nth(5) {
        candidates.push(repo.join("core").join("target").join("release").join(name));
        candidates.push(repo.join("core").join("target").join("debug").join(name));
    }
    candidates
}

impl CoreBinary {
    /// `FRAMEKEEP_CORE` first -- the same override the MCP adapter honours --
    /// then the dev layout relative to this exe. The error lists every place
    /// it looked, because "core not found" with no paths is a dead end.
    pub fn locate() -> Result<CoreBinary, String> {
        let candidates = core_candidates(
            std::env::current_exe().ok().as_deref(),
            std::env::var_os("FRAMEKEEP_CORE").map(PathBuf::from),
        );

        match candidates.iter().find(|c| c.is_file()) {
            Some(bin) => Ok(CoreBinary {
                bin: bin.clone(),
                models_dir: crate::settings::load().0.transcription.models_dir,
            }),
            None => Err(format!(
                "Couldn't find framekeep-core. Looked at: {}. \
                 Build it with `cargo build --release` in core/, or set FRAMEKEEP_CORE.",
                candidates
                    .iter()
                    .map(|c| c.display().to_string())
                    .collect::<Vec<_>>()
                    .join(" · ")
            )),
        }
    }
}

impl Runner for CoreBinary {
    fn models_dir(&self) -> Option<PathBuf> {
        self.models_dir.clone()
    }

    fn run(&self, args: &[String]) -> Result<String, String> {
        let mut cmd = std::process::Command::new(&self.bin);
        cmd.args(args);
        if let Some(dir) = self.models_dir() {
            cmd.env("FRAMEKEEP_MODELS_DIR", dir);
        }
        // This process has no console, so a console-subsystem child would be
        // handed a fresh visible one. The owner's desktop filled with empty
        // Terminal windows -- one per map and transcribe -- before this flag.
        // core applies the same flag to ffmpeg and whisper underneath it.
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let output = cmd
            .output()
            .map_err(|e| format!("Couldn't start framekeep-core ({e})."))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let said = stderr.trim();
            return Err(if said.is_empty() {
                format!("framekeep-core {} failed without saying why.", args[0])
            } else {
                said.to_string()
            });
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

/// Handles currently being processed, so a double paste cannot run the whole
/// pipeline twice for one video. Cheap and process-local: two *processes*
/// racing is already settled further down -- core's transcript store hands out
/// leases, and frame extraction is idempotent.
static IN_FLIGHT: Mutex<Option<std::collections::HashSet<String>>> = Mutex::new(None);

/// Run the pipeline for one recording on a background thread.
///
/// `notify` is whatever the caller uses to repaint -- the GUI passes an event
/// emitter, tests pass a channel, trayd passes nothing much.
pub fn kick(handle: String, notify: impl Fn(&str) + Send + Sync + 'static) {
    {
        let mut guard = IN_FLIGHT.lock().unwrap();
        let set = guard.get_or_insert_with(Default::default);
        if !set.insert(handle.clone()) {
            return; // Already on its way.
        }
    }

    std::thread::spawn(move || {
        let done = |h: &str| {
            if let Some(set) = IN_FLIGHT.lock().unwrap().as_mut() {
                set.remove(h);
            }
        };

        let queue = match Queue::open() {
            Ok(q) => q,
            Err(_) => return done(&handle),
        };
        // Read at scan time, not at enqueue time: a word added while a long
        // transcript is running still protects the recording that is running.
        let saved = crate::settings::load().0;
        match CoreBinary::locate() {
            Ok(runner) => process(
                &queue,
                &handle,
                &runner,
                &saved.redaction.patterns,
                &saved.transcription,
                &notify,
            ),
            Err(message) => {
                // No core, no frames -- but never a silent stall. The row says
                // what is missing and what to do, in the row itself.
                if let Ok(Some(mut row)) = queue.get(&handle) {
                    row.status = Status::Error;
                    row.error = Some(message);
                    let _ = queue.put(&row);
                    notify(&handle);
                }
            }
        }
        done(&handle);
    });
}

/// Is this path plausibly still being written? Used by callers that want to
/// wait for a recorder to finish -- not used yet, kept out until it is.
#[allow(dead_code)]
fn _placeholder(_: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::retention::Origin;
    use std::sync::atomic::{AtomicU32, Ordering};

    static N: AtomicU32 = AtomicU32::new(0);

    fn fixture(name: &str) -> (Queue, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "framekeep-pipeline-{}-{}-{name}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let queue = Queue::open_at(root.join("queue.db"), root.join("cache")).unwrap();
        (queue, root)
    }

    fn ingest(queue: &Queue, handle: &str) -> Recording {
        let r = Recording::new(
            handle,
            format!("C:/videos/{handle}.mp4"),
            Origin::Referenced,
            0,
        );
        queue.put(&r).unwrap();
        r
    }

    /// Answers per command name, and records the order it was asked in. The
    /// `--scan` map call is told apart from the plain one, because the honest
    /// order -- frames first, scan after the transcript -- is behaviour under
    /// test, not an accident of implementation.
    struct Script {
        map: Result<String, String>,
        /// What the `--scan` call's `scan` field holds. `None` omits the field
        /// entirely, which is an older core answering.
        scan: Option<String>,
        transcribe: Result<String, String>,
        /// What `doctor --json` answers. Healthy by default, so every test
        /// written before the speech gate keeps meaning what it meant.
        doctor: Result<String, String>,
        asked: Mutex<Vec<String>>,
        /// Every call's full argv, paired with the label in `asked`. The
        /// labels answer "in what order"; these answer "carrying what".
        argv: Mutex<Vec<Vec<String>>>,
    }

    impl Script {
        /// Builds the `map` answer around the video fields under test. The
        /// default scan is a clean one: engine present, nothing found.
        fn new(video: &str) -> Script {
            Script {
                map: Ok(format!(
                    r#"{{"video":{video},"frames":[{{"pts_time":0.0,"file":"a.webp"}},{{"pts_time":5.0,"file":"b.webp"}}]}}"#
                )),
                scan: Some(
                    r#"{"engine":{"available":true},"frames":[],"detections_total":0,"unlocated_total":0,"unreadable_frames":0}"#
                        .into(),
                ),
                transcribe: Ok("{}".into()),
                doctor: Ok(r#"{"whisper":{"available":true}}"#.into()),
                asked: Mutex::new(Vec::new()),
                argv: Mutex::new(Vec::new()),
            }
        }

        /// The argv of the call `asked` recorded under this label.
        fn args_for(&self, label: &str) -> Vec<String> {
            let at = self
                .asked
                .lock()
                .unwrap()
                .iter()
                .position(|a| a == label)
                .unwrap_or_else(|| panic!("core was never asked for `{label}`"));
            self.argv.lock().unwrap()[at].clone()
        }
    }

    impl Runner for Script {
        fn run(&self, args: &[String]) -> Result<String, String> {
            self.argv.lock().unwrap().push(args.to_vec());
            match args[0].as_str() {
                "map" if args.contains(&"--scan".to_string()) => {
                    self.asked.lock().unwrap().push("map --scan".into());
                    let base = self.map.clone()?;
                    Ok(match &self.scan {
                        Some(scan) => base.replacen(
                            r#"{"video":"#,
                            &format!(r#"{{"scan":{scan},"video":"#),
                            1,
                        ),
                        None => base,
                    })
                }
                "map" => {
                    self.asked.lock().unwrap().push("map".into());
                    assert!(
                        args.contains(&"--skip-transcript".to_string()),
                        "map without --skip-transcript would sit through the slow half twice"
                    );
                    self.map.clone()
                }
                "transcribe" => {
                    self.asked.lock().unwrap().push("transcribe".into());
                    self.transcribe.clone()
                }
                "doctor" => {
                    self.asked.lock().unwrap().push("doctor".into());
                    self.doctor.clone()
                }
                other => panic!("pipeline asked core for `{other}`"),
            }
        }
    }

    fn stages_seen(queue: &Queue, handle: &str, runner: &dyn Runner) -> Vec<&'static str> {
        let seen = Mutex::new(Vec::new());
        process(queue, handle, runner, &[], &Default::default(), &|h| {
            let status = queue.get(h).unwrap().unwrap().status;
            seen.lock().unwrap().push(status.as_str());
        });
        seen.into_inner().unwrap()
    }

    /// S6.1. An installed app has core beside it, and until S6.1 nothing did.
    ///
    /// Pinned rather than left for the packaging step to discover, because the
    /// symptom misleads: the app says "couldn't find framekeep-core" and lists
    /// paths inside a source tree that is not on the machine, which reads like
    /// a broken install rather than a lookup that never knew about installs.
    ///
    /// Paths are built with `join` and the binary name comes from the same
    /// `cfg!` the code uses, so this says the same thing on either platform.
    /// The first version wrote a `C:\...` literal and expected `.exe`; the
    /// Linux job failed it, which is exactly the job's purpose -- backslashes
    /// are ordinary characters there and the binary has no extension.
    #[test]
    fn an_installed_app_finds_core_beside_itself() {
        let dir = PathBuf::from("install").join("Framekeep_1.0.0.0_x64__abc");
        let found = core_candidates(Some(&dir.join("framekeep-tray.exe")), None);
        assert_eq!(
            found.first(),
            Some(&dir.join(core_name())),
            "an installed app looks beside itself first: {found:?}"
        );
    }

    /// And neither of the two paths that already worked was traded for it.
    #[test]
    fn the_dev_tree_still_resolves_and_the_override_still_wins() {
        let repo = PathBuf::from("repo");
        let dev = repo
            .join("tray")
            .join("src-tauri")
            .join("target")
            .join("release")
            .join("framekeep-tray.exe");

        let found = core_candidates(Some(&dev), None);
        let expected = repo
            .join("core")
            .join("target")
            .join("release")
            .join(core_name());
        assert!(
            found.contains(&expected),
            "the dev layout was lost: {found:?}"
        );
        // Beside-the-exe is listed first and is harmless here: nothing sits
        // there in a dev tree, so the dev path below it is what resolves.
        assert!(found.len() > 1, "the dev candidates vanished: {found:?}");

        let mine = PathBuf::from("elsewhere").join("core");
        let overridden = core_candidates(Some(&dev), Some(mine.clone()));
        assert_eq!(overridden[0], mine, "FRAMEKEEP_CORE stopped winning");
    }

    /// S5.8. The words a person typed reach core, and reach only the call that
    /// scans -- `transcribe` has no use for them and the first `map` has not
    /// got to the frames yet. Pinned because the flag is the entire contract
    /// between the Settings screen and the scanner: get it wrong and the
    /// feature is silently off, which is the failure a person cannot see.
    #[test]
    fn a_persons_own_words_ride_along_with_the_scan_and_nothing_else() {
        let (queue, _root) = fixture("patterns");
        ingest(&queue, "a");
        let script =
            Script::new(r#"{"duration_seconds":24.5,"width":1624,"height":860,"has_audio":true}"#);

        let typed = vec![
            "Project Nightingale".to_string(),
            "MA NHAN VIEN".to_string(),
        ];
        process(&queue, "a", &script, &typed, &Default::default(), &|_| {});

        let scan_args = script.args_for("map --scan");
        // Each word its own array element, never joined into a string: they
        // carry spaces by design, and the ffmpeg lesson about arguments with
        // spaces in them applies to every process this app spawns.
        for word in &typed {
            let at = scan_args
                .iter()
                .position(|a| a == word)
                .unwrap_or_else(|| panic!("{word:?} never reached core: {scan_args:?}"));
            assert_eq!(scan_args[at - 1], "--pattern");
        }
        for other in ["map", "transcribe"] {
            let args = script.args_for(other);
            assert!(
                !args.iter().any(|a| a == "--pattern"),
                "`{other}` was handed a pattern it has no use for: {args:?}"
            );
        }
    }

    /// Nobody has typed anything, which is the ordinary case. The scan call is
    /// then byte-for-byte the one the measurements were taken with.
    #[test]
    fn with_no_words_typed_the_scan_call_is_exactly_what_it_always_was() {
        let (queue, _root) = fixture("no-patterns");
        ingest(&queue, "a");
        let script =
            Script::new(r#"{"duration_seconds":24.5,"width":1624,"height":860,"has_audio":true}"#);
        process(&queue, "a", &script, &[], &Default::default(), &|_| {});
        assert!(!script
            .args_for("map --scan")
            .iter()
            .any(|a| a == "--pattern"));
    }

    #[test]
    fn a_recording_with_speech_walks_every_honest_stage() {
        let (queue, _root) = fixture("speech");
        ingest(&queue, "a");
        let script =
            Script::new(r#"{"duration_seconds":24.5,"width":1624,"height":860,"has_audio":true}"#);

        let stages = stages_seen(&queue, "a", &script);
        // Scanning sits after the transcript: it needs the frames, and those
        // are already on disk, so the person sees the slow half first.
        assert_eq!(
            stages,
            ["extracting_frames", "transcribing", "scanning", "ready"]
        );

        let row = queue.get("a").unwrap().unwrap();
        assert_eq!(row.duration_ms, Some(24_500));
        assert_eq!(row.width, Some(1624));
        assert_eq!(row.frame_count, Some(2));
        assert_eq!(row.status, Status::Ready);
        assert_eq!(
            row.finding_count,
            Some(0),
            "a clean scan is 0, never NULL -- somebody looked"
        );
        assert_eq!(
            *script.asked.lock().unwrap(),
            ["map", "doctor", "transcribe", "map --scan"]
        );
    }

    /// The first paste on a clean install: a video with sound, no model yet.
    /// S6.4's clean-machine run found this as an error row -- the pipeline
    /// sank the whole recording over equipment the Settings copy calls
    /// optional. Frames-only is the honest degrade.
    #[test]
    fn a_missing_speech_model_costs_the_transcript_not_the_recording() {
        let (queue, _root) = fixture("no-model");
        ingest(&queue, "a");
        let mut script =
            Script::new(r#"{"duration_seconds":10.0,"width":1920,"height":1080,"has_audio":true}"#);
        script.doctor = Ok(
            r#"{"whisper":{"available":false,"unavailable":"No speech model installed"}}"#.into(),
        );
        script.transcribe = Err("transcribe must not have been asked".into());

        let stages = stages_seen(&queue, "a", &script);
        assert_eq!(
            stages,
            ["extracting_frames", "scanning", "ready"],
            "no transcribing stage, and no error"
        );

        let row = queue.get("a").unwrap().unwrap();
        assert_eq!(row.status, Status::Ready);
        assert_eq!(row.duration_ms, Some(10_000), "frames still arrived");
        assert!(
            !script
                .asked
                .lock()
                .unwrap()
                .contains(&"transcribe".to_string()),
            "a machine that cannot transcribe was asked to anyway"
        );
    }

    /// A stage failure must not erase what earlier stages earned. The
    /// clean-machine error rows showed "--" for duration on recordings whose
    /// frames had extracted fine, because the error path wrote back the
    /// pre-stage snapshot -- and the missing metadata sent the diagnosis
    /// hunting a map failure that never happened.
    #[test]
    fn an_error_row_keeps_the_metadata_the_map_stage_already_earned() {
        let (queue, _root) = fixture("keep-meta");
        ingest(&queue, "a");
        let mut script =
            Script::new(r#"{"duration_seconds":10.0,"width":1920,"height":1080,"has_audio":true}"#);
        script.transcribe = Err("whisper fell over".into());

        process(&queue, "a", &script, &[], &Default::default(), &|_| {});

        let row = queue.get("a").unwrap().unwrap();
        assert_eq!(row.status, Status::Error);
        assert_eq!(row.error.as_deref(), Some("whisper fell over"));
        assert_eq!(
            row.duration_ms,
            Some(10_000),
            "the error path threw away the map stage's answer"
        );
        assert_eq!(row.frame_count, Some(2));
    }

    /// And the doctor being unreachable is the same answer, not a new error:
    /// anything short of a confident yes degrades to frames-only.
    #[test]
    fn an_unreachable_doctor_reads_as_no_speech_not_as_a_failure() {
        let (queue, _root) = fixture("no-doctor");
        ingest(&queue, "a");
        let mut script =
            Script::new(r#"{"duration_seconds":10.0,"width":1920,"height":1080,"has_audio":true}"#);
        script.doctor = Err("Couldn't run framekeep-core.".into());
        script.transcribe = Err("transcribe must not have been asked".into());

        process(&queue, "a", &script, &[], &Default::default(), &|_| {});
        let row = queue.get("a").unwrap().unwrap();
        assert_eq!(row.status, Status::Ready);
    }

    #[test]
    fn a_silent_recording_skips_the_transcribing_stage_entirely() {
        let (queue, _root) = fixture("silent");
        ingest(&queue, "a");
        let script = Script::new(
            r#"{"duration_seconds":10.0,"width":1920,"height":1080,"has_audio":false}"#,
        );

        let stages = stages_seen(&queue, "a", &script);
        assert!(!stages.contains(&"transcribing"), "{stages:?}");
        assert_eq!(queue.get("a").unwrap().unwrap().status, Status::Ready);
        // Whisper was never consulted about silence -- that is core's rule,
        // and the pipeline must not undo it by asking anyway.
        assert_eq!(*script.asked.lock().unwrap(), ["map", "map --scan"]);
    }

    #[test]
    fn findings_park_the_row_at_needs_review_with_the_scan_saved() {
        let (queue, root) = fixture("findings");
        ingest(&queue, "a");
        let mut script = Script::new(
            r#"{"duration_seconds":10.0,"width":1920,"height":1080,"has_audio":false}"#,
        );
        script.scan = Some(
            r#"{"engine":{"available":true},"frames":[{"file":"a.webp","detections":[
                {"kind":"API key","masked":"sk-••••••W4aZ","located":true,
                 "boxes":[{"x":1.0,"y":2.0,"w":3.0,"h":4.0}]}]}],
                "detections_total":1,"unlocated_total":0,"unreadable_frames":0}"#
                .into(),
        );

        let stages = stages_seen(&queue, "a", &script);
        assert_eq!(stages.last(), Some(&"needs_review"));

        let row = queue.get("a").unwrap().unwrap();
        assert_eq!(row.finding_count, Some(1));
        // The evidence is on disk where the review screen will read it, and it
        // parses as the shape review.rs acts on -- saved and readable are two
        // different claims.
        let saved = crate::review::load_scan(&queue.cache_dir("a")).unwrap();
        assert_eq!(saved.detections_total, 1);
        assert_eq!(saved.frames[0].detections[0].masked, "sk-••••••W4aZ");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn no_engine_means_nobody_looked_and_the_count_stays_null() {
        let (queue, _root) = fixture("noengine");
        ingest(&queue, "a");
        let mut script = Script::new(
            r#"{"duration_seconds":10.0,"width":1920,"height":1080,"has_audio":false}"#,
        );
        script.scan = Some(
            r#"{"engine":{"available":false,"reason":"no OCR language pack"},
                "frames":[],"detections_total":0,"unlocated_total":0,"unreadable_frames":0}"#
                .into(),
        );

        stages_seen(&queue, "a", &script);
        let row = queue.get("a").unwrap().unwrap();
        assert_eq!(row.status, Status::Ready, "no engine must not trap the row");
        assert_eq!(
            row.finding_count, None,
            "nobody looked; promoting that to `looked and found nothing` is the lie \
             the standalone warning exists to prevent"
        );
    }

    #[test]
    fn an_older_core_without_scan_support_still_finishes() {
        let (queue, _root) = fixture("oldcore");
        ingest(&queue, "a");
        let mut script = Script::new(
            r#"{"duration_seconds":10.0,"width":1920,"height":1080,"has_audio":false}"#,
        );
        script.scan = None; // the `scan` field simply absent from map's answer

        stages_seen(&queue, "a", &script);
        let row = queue.get("a").unwrap().unwrap();
        assert_eq!(row.status, Status::Ready);
        assert_eq!(row.finding_count, None);
    }

    #[test]
    fn a_failure_lands_in_the_row_with_cores_own_sentence() {
        let (queue, _root) = fixture("failure");
        ingest(&queue, "a");
        let mut script =
            Script::new(r#"{"duration_seconds":10.0,"width":1920,"height":1080,"has_audio":true}"#);
        script.map =
            Err("ffmpeg couldn't read that file. It may be corrupted or still recording.".into());

        process(&queue, "a", &script, &[], &Default::default(), &|_| {});

        let row = queue.get("a").unwrap().unwrap();
        assert_eq!(row.status, Status::Error);
        assert_eq!(
            row.error.as_deref(),
            Some("ffmpeg couldn't read that file. It may be corrupted or still recording.")
        );
    }

    #[test]
    fn unreadable_core_output_is_an_error_not_a_panic() {
        let (queue, _root) = fixture("garbage");
        ingest(&queue, "a");
        let script = Script::new("this is not json");
        process(&queue, "a", &script, &[], &Default::default(), &|_| {});
        let row = queue.get("a").unwrap().unwrap();
        assert_eq!(row.status, Status::Error);
        assert!(row.error.unwrap().contains("unreadable"));
    }

    #[test]
    fn a_row_removed_before_its_turn_is_left_alone() {
        let (queue, _root) = fixture("gone");
        let script = Script::new("{}");
        process(
            &queue,
            "never-ingested",
            &script,
            &[],
            &Default::default(),
            &|h| panic!("notified about {h}, which does not exist"),
        );
        assert!(
            script.asked.lock().unwrap().is_empty(),
            "core was consulted for a missing row"
        );
    }
}
