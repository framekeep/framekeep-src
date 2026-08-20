//! Integration tests: these run the real binary against real video files.
//!
//! Every condition here was previously verified by hand in a terminal, once,
//! and never again. That is the gap this file closes -- the unit tests cover
//! the reasoning, but nothing was checking that `framekeep-core probe` actually
//! survives a path with Vietnamese diacritics in it.
//!
//! Fixtures are generated with ffmpeg rather than committed, so the repo stays
//! free of binary test data and the fixtures always match the tools in use.
//!
//! Tests **skip** rather than fail when ffmpeg is absent: a contributor without
//! the vendored toolchain should still be able to run `cargo test`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_framekeep-core");

/// Finds ffmpeg the same way the product does, so tests exercise the real
/// discovery path rather than assuming a location.
fn ffmpeg() -> Option<PathBuf> {
    let name = if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    };

    if let Some(dir) = std::env::var_os("FRAMEKEEP_FFMPEG_DIR") {
        let p = PathBuf::from(dir).join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    // Walk up from the test binary looking for vendor/ffmpeg/<arch>.
    let arch = if cfg!(target_arch = "aarch64") {
        "winarm64"
    } else {
        "win64"
    };
    let mut dir: Option<PathBuf> = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf));
    while let Some(d) = dir {
        let p = d.join("vendor").join("ffmpeg").join(arch).join(name);
        if p.is_file() {
            return Some(p);
        }
        dir = d.parent().map(Path::to_path_buf);
    }
    // Last resort: PATH.
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|d| d.join(name))
            .find(|p| p.is_file())
    })
}

/// `None` means "skip": no toolchain, so nothing here can be tested honestly.
macro_rules! need_ffmpeg {
    () => {
        match ffmpeg() {
            Some(f) => f,
            None => {
                eprintln!("skipping: no ffmpeg found (set FRAMEKEEP_FFMPEG_DIR)");
                return;
            }
        }
    };
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        // Unique per test without pulling in a rand crate: the test name plus
        // the process id is enough, and it keeps runs isolated.
        let p = std::env::temp_dir().join(format!("framekeep-it-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).expect("temp dir");
        TempDir(p)
    }
    fn join(&self, s: &str) -> PathBuf {
        self.0.join(s)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run_ffmpeg(ff: &Path, args: &[&std::ffi::OsStr]) {
    let out = Command::new(ff)
        .args(args)
        .output()
        .expect("ffmpeg should start");
    assert!(
        out.status.success(),
        "fixture generation failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A plain SDR clip with a moving pattern, optionally silent.
fn make_video(ff: &Path, dest: &Path, seconds: &str, with_audio: bool) {
    use std::ffi::OsStr;
    let mut args: Vec<&OsStr> = vec![
        "-y".as_ref(),
        "-hide_banner".as_ref(),
        "-loglevel".as_ref(),
        "error".as_ref(),
        "-f".as_ref(),
        "lavfi".as_ref(),
        "-i".as_ref(),
        "testsrc=size=640x360:rate=15".as_ref(),
    ];
    if with_audio {
        args.extend_from_slice(&[
            "-f".as_ref(),
            "lavfi".as_ref(),
            "-i".as_ref(),
            "sine=frequency=440".as_ref(),
            "-c:a".as_ref(),
            "aac".as_ref(),
            "-shortest".as_ref(),
        ]);
    }
    args.extend_from_slice(&[
        "-t".as_ref(),
        seconds.as_ref(),
        "-pix_fmt".as_ref(),
        "yuv420p".as_ref(),
        dest.as_os_str(),
    ]);
    run_ffmpeg(ff, &args);
}

fn core(args: &[&std::ffi::OsStr]) -> Output {
    Command::new(BIN)
        .args(args)
        .output()
        .expect("framekeep-core should start")
}

/// Runs the binary with its cache pointed at a folder of our own.
///
/// The cache root comes from `USERPROFILE`/`HOME`, so overriding those gives a
/// test its own. That matters for anything that inspects the cache: tests run in
/// parallel, and two of them transcribing at once against the shared real cache
/// made one see the other's scratch file and fail. It failed, then passed, which
/// is the worst way for a test to behave -- it teaches you to re-run instead of
/// to look.
fn core_in(home: &Path, args: &[&std::ffi::OsStr]) -> Output {
    Command::new(BIN)
        .args(args)
        .env("USERPROFILE", home)
        .env("HOME", home)
        .output()
        .expect("framekeep-core should start")
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

// ---------------------------------------------------------------------------

/// The mandatory test case from CLAUDE.md. Both competitors audited in S0.4
/// break on Windows path handling; this is the one that must never regress.
#[test]
fn probe_survives_a_path_with_vietnamese_diacritics_and_spaces() {
    let ff = need_ffmpeg!();
    let tmp = TempDir::new("unicode");
    let dir = tmp.join("Nguyễn Văn A").join("My Videos");
    std::fs::create_dir_all(&dir).unwrap();
    let video = dir.join("test video.mp4");
    make_video(&ff, &video, "1", true);

    let out = core(&["probe".as_ref(), video.as_os_str()]);
    assert!(out.status.success(), "probe failed:\n{}", stderr(&out));

    let json = stdout(&out);
    assert!(
        json.contains("\"width\": 640"),
        "unexpected output:\n{json}"
    );
    assert!(
        json.contains("\"has_audio\": true"),
        "unexpected output:\n{json}"
    );
}

#[test]
fn a_silent_recording_is_reported_not_rejected() {
    let ff = need_ffmpeg!();
    let tmp = TempDir::new("silent");
    let video = tmp.join("silent.mp4");
    make_video(&ff, &video, "1", false);

    let out = core(&["probe".as_ref(), video.as_os_str()]);
    assert!(
        out.status.success(),
        "a silent video must not be an error:\n{}",
        stderr(&out)
    );
    assert!(stdout(&out).contains("\"has_audio\": false"));
}

/// A recording still being written is a normal thing for this product to meet,
/// and it must be told apart from a file that is not a video at all.
#[test]
fn an_interrupted_recording_and_a_text_file_get_different_messages() {
    let ff = need_ffmpeg!();
    let tmp = TempDir::new("broken");

    let whole = tmp.join("whole.mp4");
    make_video(&ff, &whole, "1", false);
    let bytes = std::fs::read(&whole).unwrap();
    let truncated = tmp.join("interrupted.mp4");
    std::fs::write(&truncated, &bytes[..bytes.len() * 2 / 5]).unwrap();

    let not_video = tmp.join("notes.txt");
    std::fs::write(&not_video, b"this is not a video").unwrap();

    let a = core(&["probe".as_ref(), truncated.as_os_str()]);
    assert!(!a.status.success(), "a truncated file must fail");
    assert!(
        stderr(&a).contains("still running") || stderr(&a).contains("interrupted"),
        "an interrupted recording should say so:\n{}",
        stderr(&a)
    );

    let b = core(&["probe".as_ref(), not_video.as_os_str()]);
    assert!(!b.status.success(), "a text file must fail");
    assert!(stderr(&b).contains("isn't a video"), "got:\n{}", stderr(&b));
    assert!(
        !stderr(&b).contains("still running"),
        "a text file is not a recording in progress:\n{}",
        stderr(&b)
    );
}

#[test]
fn a_missing_file_says_what_to_do_next() {
    let out = core(&["probe".as_ref(), "C:/definitely/not/here.mp4".as_ref()]);
    assert!(!out.status.success());
    let msg = stderr(&out);
    assert!(msg.contains("no file"), "got:\n{msg}");
    assert!(
        msg.contains("try again"),
        "every error names a next step:\n{msg}"
    );
}

/// Frame extraction end to end, including that stdout stays machine-readable.
#[test]
fn frames_writes_pngs_and_reports_them_as_json() {
    let ff = need_ffmpeg!();
    let tmp = TempDir::new("frames");
    let video = tmp.join("clip.mp4");
    make_video(&ff, &video, "20", false);
    let out_dir = tmp.join("out");

    let out = core(&[
        "frames".as_ref(),
        video.as_os_str(),
        "--out".as_ref(),
        out_dir.as_os_str(),
    ]);
    assert!(out.status.success(), "frames failed:\n{}", stderr(&out));

    let json = stdout(&out);
    assert!(json.starts_with('{'), "stdout must be JSON:\n{json}");
    assert!(json.contains("\"frames\""));

    let written = std::fs::read_dir(&out_dir)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|x| x == "png"))
        .count();
    assert!(written > 0, "no PNGs were written");
}

/// Running twice into the same folder must not accumulate stale frames -- the
/// cache directory is reused every time a video is re-processed.
#[test]
fn a_second_run_replaces_frames_rather_than_piling_up() {
    let ff = need_ffmpeg!();
    let tmp = TempDir::new("rerun");
    let video = tmp.join("clip.mp4");
    make_video(&ff, &video, "20", false);
    let out_dir = tmp.join("out");

    let count = |d: &Path| {
        std::fs::read_dir(d)
            .map(|it| {
                it.filter_map(Result::ok)
                    .filter(|e| e.path().extension().is_some_and(|x| x == "png"))
                    .count()
            })
            .unwrap_or(0)
    };

    let args: Vec<&std::ffi::OsStr> = vec![
        "frames".as_ref(),
        video.as_os_str(),
        "--out".as_ref(),
        out_dir.as_os_str(),
    ];
    assert!(core(&args).status.success());
    let first = count(&out_dir);
    assert!(
        core(&args).status.success(),
        "the second run must succeed too"
    );
    assert_eq!(count(&out_dir), first, "frames accumulated across runs");
}

/// `--out` may be a folder the user cares about. Clearing a previous run must
/// never reach beyond the files this tool wrote.
#[test]
fn a_rerun_leaves_unrelated_files_alone() {
    let ff = need_ffmpeg!();
    let tmp = TempDir::new("neighbours");
    let video = tmp.join("clip.mp4");
    make_video(&ff, &video, "20", false);
    let out_dir = tmp.join("out");
    std::fs::create_dir_all(&out_dir).unwrap();
    let precious = out_dir.join("holiday.png");
    std::fs::write(&precious, b"not ours").unwrap();

    let args: Vec<&std::ffi::OsStr> = vec![
        "frames".as_ref(),
        video.as_os_str(),
        "--out".as_ref(),
        out_dir.as_os_str(),
    ];
    assert!(core(&args).status.success());
    assert!(core(&args).status.success());

    assert!(precious.is_file(), "an unrelated file was deleted");
    assert_eq!(std::fs::read(&precious).unwrap(), b"not ours");
}

/// iPhones on iOS 18+ record HDR by default. Without tone-mapping the frames
/// come out washed out, and nothing in the output says so.
#[test]
fn hdr_sources_are_detected() {
    let ff = need_ffmpeg!();
    let tmp = TempDir::new("hdr");
    let video = tmp.join("hdr.mp4");

    use std::ffi::OsStr;
    let pq = "format=yuv420p,zscale=tin=bt709:min=bt709:pin=bt709:rin=tv:\
              t=smpte2084:m=bt2020nc:p=bt2020:r=tv,format=yuv420p10le";
    let args: Vec<&OsStr> = vec![
        "-y".as_ref(),
        "-hide_banner".as_ref(),
        "-loglevel".as_ref(),
        "error".as_ref(),
        "-f".as_ref(),
        "lavfi".as_ref(),
        "-i".as_ref(),
        "testsrc2=size=320x180:rate=15".as_ref(),
        "-t".as_ref(),
        "1".as_ref(),
        "-vf".as_ref(),
        pq.as_ref(),
        "-c:v".as_ref(),
        "libvpx-vp9".as_ref(),
        "-b:v".as_ref(),
        "1M".as_ref(),
        "-color_primaries".as_ref(),
        "bt2020".as_ref(),
        "-color_trc".as_ref(),
        "smpte2084".as_ref(),
        "-colorspace".as_ref(),
        "bt2020nc".as_ref(),
        video.as_os_str(),
    ];
    let made = Command::new(&ff)
        .args(&args)
        .output()
        .expect("ffmpeg should start");
    if !made.status.success() {
        // zscale or vp9 may be missing from a contributor's ffmpeg build.
        eprintln!("skipping: this ffmpeg cannot build an HDR fixture");
        return;
    }

    let out = core(&["probe".as_ref(), video.as_os_str()]);
    assert!(out.status.success(), "probe failed:\n{}", stderr(&out));
    let json = stdout(&out);
    assert!(
        json.contains("\"is_hdr\": true"),
        "HDR was not detected:\n{json}"
    );
    assert!(
        json.contains("smpte2084"),
        "transfer curve missing:\n{json}"
    );
}

/// The map is the thing a model reads first, so it must never carry pixels.
#[test]
fn the_map_lists_frames_by_path_and_never_inlines_them() {
    let ff = need_ffmpeg!();
    let tmp = TempDir::new("map");
    let video = tmp.join("clip.mp4");
    make_video(&ff, &video, "20", false);
    let out_dir = tmp.join("out");

    let out = core(&[
        "map".as_ref(),
        video.as_os_str(),
        "--out".as_ref(),
        out_dir.as_os_str(),
        "--skip-transcript".as_ref(),
    ]);
    assert!(out.status.success(), "map failed:\n{}", stderr(&out));

    let json = stdout(&out);
    assert!(json.contains("\"frames\""));
    assert!(
        json.contains(".png"),
        "frames should be listed by path:\n{json}"
    );
    assert!(
        !json.contains("base64"),
        "the map must never inline image data"
    );
    assert!(
        json.contains("\"transcript\": null"),
        "--skip-transcript should say so:\n{json}"
    );
}

/// Silence must not cost a model load.
///
/// `transcribe` probes the file before it looks for whisper, so a recording
/// with no audio track never touches the 547 MiB default. This test proves it
/// the only way that cannot be faked: it points `--model` at a path that does
/// not exist. If the command ever starts locating whisper first, this fails.
///
/// It also needs no model, which is why it runs everywhere `cargo test` does.
#[test]
fn a_silent_recording_never_reaches_the_model() {
    let ff = need_ffmpeg!();
    let tmp = TempDir::new("silent-transcribe");
    let video = tmp.join("silent.mp4");
    make_video(&ff, &video, "1", false);

    let out = core(&[
        "transcribe".as_ref(),
        video.as_os_str(),
        "--model".as_ref(),
        tmp.join("there-is-no-model-here.bin").as_os_str(),
    ]);

    assert!(
        out.status.success(),
        "a silent recording is ordinary, not an error:\n{}",
        stderr(&out)
    );
    let json = stdout(&out);
    assert!(json.contains("\"has_audio\": false"), "got:\n{json}");
    assert!(
        json.contains("\"segments\": []"),
        "silence transcribes to nothing, not to a guess:\n{json}"
    );
}

/// The privacy promise in S1.5, tested rather than asserted in a comment.
///
/// Transcription writes a 16 kHz copy of the user's audio to disk. That copy
/// has no reason to outlive the job, and nothing was checking that it does not
/// -- a promise about someone's recorded voice is exactly the kind that has to
/// be enforced by a test rather than by good intentions.
///
/// Skips without whisper or a model; CI installs both and asserts it ran.
#[test]
fn transcribing_leaves_no_copy_of_the_audio_behind() {
    let ff = need_ffmpeg!();
    let (_whisper_dir, model) = match whisper_and_model() {
        Some(w) => w,
        None => {
            eprintln!("skipping: no whisper-cli or no model (set FRAMEKEEP_TEST_MODEL)");
            return;
        }
    };

    let tmp = TempDir::new("transcribe-audio");
    let video = tmp.join("talk.mp4");
    make_video(&ff, &video, "2", true);

    // Its own cache, so nothing another test does can be mistaken for a leak
    // here, and nothing this test leaves behind can accuse another one.
    let home = tmp.join("home");
    let out = core_in(
        &home,
        &[
            "transcribe".as_ref(),
            video.as_os_str(),
            "--model".as_ref(),
            model.as_os_str(),
        ],
    );

    assert!(out.status.success(), "transcribe failed:\n{}", stderr(&out));
    let json = stdout(&out);
    assert!(json.contains("\"has_audio\": true"), "got:\n{json}");

    let leftovers = stray_audio_copies(&home);
    assert!(
        leftovers.is_empty(),
        "the extracted audio must be deleted after transcription, found: {leftovers:?}"
    );
}

/// The same promise, on the path where it used to break.
///
/// The delete used to sit at the end of the happy path, so any failure left the
/// user's audio on disk. Here the model file does not exist, so whisper fails
/// after ffmpeg has already written the copy -- exactly the case that leaked.
#[test]
fn a_failed_transcription_still_deletes_the_audio() {
    let ff = need_ffmpeg!();
    let tmp = TempDir::new("transcribe-fail");
    let video = tmp.join("talk.mp4");
    make_video(&ff, &video, "1", true);

    let home = tmp.join("home");
    let out = core_in(
        &home,
        &[
            "transcribe".as_ref(),
            video.as_os_str(),
            "--model".as_ref(),
            tmp.join("not-a-model.bin").as_os_str(),
        ],
    );
    assert!(!out.status.success(), "a missing model must fail loudly");

    let leftovers = stray_audio_copies(&home);
    assert!(
        leftovers.is_empty(),
        "a failed transcription must not leave the user's audio behind: {leftovers:?}"
    );
}

/// The status check must never be the thing that starts two minutes of work.
///
/// An MCP adapter polls this while the model is already looking at frames. If
/// asking "is it ready" quietly kicked off transcription, every poll would
/// start another one.
#[test]
fn asking_whether_a_transcript_is_ready_starts_nothing() {
    let ff = need_ffmpeg!();
    let tmp = TempDir::new("transcript-status");
    let video = tmp.join("fresh.mp4");
    make_video(&ff, &video, "1", true);

    let home = tmp.join("home");
    let out = core_in(&home, &["transcript".as_ref(), video.as_os_str()]);
    assert!(
        out.status.success(),
        "status check failed:\n{}",
        stderr(&out)
    );
    assert!(
        stdout(&out).contains("\"status\": \"absent\""),
        "a recording nobody has transcribed is absent, not an error:\n{}",
        stdout(&out)
    );

    // And nothing was created on the way to saying so.
    assert!(
        stray_audio_copies(&home).is_empty(),
        "a read-only status check must not extract audio"
    );
}

/// Transcribing once must be enough.
///
/// Before the store existed, every `map` of the same recording paid the full
/// transcription again -- two minutes at the default model's ~1x realtime, for
/// words that had already been produced.
#[test]
fn a_transcript_is_produced_once_and_reused() {
    let ff = need_ffmpeg!();
    let (_w, model) = match whisper_and_model() {
        Some(w) => w,
        None => {
            eprintln!("skipping: no whisper-cli or no model (set FRAMEKEEP_TEST_MODEL)");
            return;
        }
    };

    let tmp = TempDir::new("transcript-reuse");
    let video = tmp.join("spoken.mp4");
    make_video(&ff, &video, "2", true);

    let first = core(&[
        "transcribe".as_ref(),
        video.as_os_str(),
        "--model".as_ref(),
        model.as_os_str(),
    ]);
    assert!(
        first.status.success(),
        "transcribe failed:\n{}",
        stderr(&first)
    );

    let status = core(&["transcript".as_ref(), video.as_os_str()]);
    assert!(
        stdout(&status).contains("\"status\": \"ready\""),
        "after transcribing, the status must be ready:\n{}",
        stdout(&status)
    );

    // The map is where the saving actually lands.
    let mapped = core(&[
        "map".as_ref(),
        video.as_os_str(),
        "--model".as_ref(),
        model.as_os_str(),
    ]);
    assert!(mapped.status.success(), "map failed:\n{}", stderr(&mapped));
    let json = stdout(&mapped);
    assert!(
        json.contains("\"transcript_from_cache\": true"),
        "map must reuse the stored transcript rather than redo it:\n{json}"
    );
    // Said out loud rather than inferred: a caller comparing timings between
    // runs needs to know which of the two it got.
    assert!(
        json.contains("\"transcript_seconds\": null"),
        "a cached transcript took no transcription time, and should say so:\n{json}"
    );
}

/// whisper-cli plus a model to run it with, or `None` to skip.
///
/// Prefers `tiny.en`: a test that takes two minutes is a test people turn off.
fn whisper_and_model() -> Option<(PathBuf, PathBuf)> {
    let name = if cfg!(windows) {
        "whisper-cli.exe"
    } else {
        "whisper-cli"
    };

    let mut cli = std::env::var_os("FRAMEKEEP_WHISPER_DIR")
        .map(PathBuf::from)
        .map(|d| d.join(name))
        .filter(|p| p.is_file());

    if cli.is_none() {
        let arch = if cfg!(target_arch = "aarch64") {
            "winarm64"
        } else {
            "win64"
        };
        let mut dir: Option<PathBuf> = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf));
        while let Some(d) = dir {
            let p = d.join("vendor").join("whisper").join(arch).join(name);
            if p.is_file() {
                cli = Some(p);
                break;
            }
            dir = d.parent().map(Path::to_path_buf);
        }
    }
    let cli = cli?;

    if let Some(m) = std::env::var_os("FRAMEKEEP_TEST_MODEL").map(PathBuf::from) {
        if m.is_file() {
            return Some((cli, m));
        }
    }
    let dir = home()?.join(".framekeep").join("models");
    let tiny = dir.join("ggml-tiny.en.bin");
    if tiny.is_file() {
        return Some((cli, tiny));
    }
    let any = std::fs::read_dir(&dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|e| e == "bin"))?;
    Some((cli, any))
}

fn home() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

/// Any extracted-audio file still sitting in the given cache root.
///
/// Takes the root rather than reading the environment, so a test can point it
/// at its own folder and be unaffected by whatever else is running.
fn stray_audio_copies(home: &Path) -> Vec<PathBuf> {
    let cache = home.join(".framekeep").join("cache");
    let Ok(entries) = std::fs::read_dir(&cache) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .map(|e| e.path().join("audio-16k.wav"))
        .filter(|p| p.is_file())
        .collect()
}

#[test]
fn doctor_names_the_binaries_in_use() {
    // Runs with or without a toolchain: the point is that it explains itself
    // either way rather than failing silently.
    let out = core(&["doctor".as_ref()]);
    let text = format!("{}{}", stdout(&out), stderr(&out));
    assert!(text.contains("framekeep-core"), "got:\n{text}");
    assert!(
        text.contains("ffmpeg") || text.contains("Can't find"),
        "doctor must name ffmpeg or say it is missing:\n{text}"
    );
}

#[test]
fn an_unknown_command_explains_itself_rather_than_panicking() {
    let out = core(&["definitely-not-a-command".as_ref()]);
    assert_eq!(out.status.code(), Some(2), "usage errors should exit 2");
    assert!(stderr(&out).contains("USAGE"));
}

/// The one committed binary fixture in this repo, and it earns the exception.
///
/// Everything else here is generated with ffmpeg so the fixtures always match
/// the tools in use. OCR has no such generator: the text has to be *rendered*,
/// and what the engine can read depends on how it was rendered. This is a
/// 5 KB crop of the measured corpus (`spike/s5-ocr-poc/make-corpus.py`),
/// carrying one planted key at the 16px size the measurement calls readable.
///
/// The key in it is synthetic -- it came from the corpus generator and has
/// never been a credential for anything.
fn one_key_frame() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("ocr")
        .join("one-key.png")
}

/// Marker other tooling greps for. CI asserts this line is absent whenever the
/// runner reported an OCR engine, because a scan test that quietly skips is a
/// green tick over an unexercised safety mechanism.
const NO_ENGINE: &str = "skipping: no OCR engine";

#[test]
fn scanning_a_frame_reports_the_key_masked_and_locatable() {
    let out = Command::new(BIN)
        .arg("scan")
        .arg(one_key_frame())
        .output()
        .expect("run scan");

    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        // No language pack, or not Windows. Both are facts about the machine,
        // and neither is this test's business to fail over.
        if err.contains("no OCR engine") || err.contains("Windows-only") {
            eprintln!("{NO_ENGINE}");
            return;
        }
        panic!("scan failed: {err}");
    }

    let json: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("scan must emit JSON on stdout");
    let detections = json["detections"].as_array().expect("detections array");

    assert!(
        !detections.is_empty(),
        "the planted key at 16px is inside the measured recall band and must be found"
    );

    let hit = &detections[0];
    let masked = hit["masked"].as_str().unwrap();
    assert!(masked.contains('•'), "value must arrive masked: {masked}");
    assert!(
        !masked.contains("T3Blbk") && !masked.contains("T381bk"),
        "the masked form leaks the body: {masked}"
    );
    assert_eq!(hit["located"], true, "a 16px key must come with a box");

    // The box has to land inside the picture, or nothing downstream can paint
    // with it. The fixture is 720x50.
    let b = &hit["boxes"][0];
    let (x, y, w, h) = (
        b["x"].as_f64().unwrap(),
        b["y"].as_f64().unwrap(),
        b["w"].as_f64().unwrap(),
        b["h"].as_f64().unwrap(),
    );
    assert!(x >= 0.0 && y >= 0.0, "box starts outside the frame");
    assert!(
        x + w <= 720.0 && y + h <= 50.0,
        "box {x},{y} {w}x{h} runs past the 720x50 frame"
    );
    assert!(w > 0.0 && h > 0.0, "a zero-area box covers nothing");

    // Nowhere in the output, at any depth, is the readable key.
    let whole = String::from_utf8_lossy(&out.stdout);
    assert!(
        !whole.contains("T3Blbk") && !whole.contains("T381bk"),
        "the scan output printed the secret it was asked to hide"
    );
}

#[test]
fn a_map_says_null_when_nobody_asked_to_scan_and_a_summary_when_they_did() {
    let Some(ff) = ffmpeg() else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let tmp = TempDir::new("scan-flag");
    let video = tmp.join("clip.mp4");
    make_video(&ff, &video, "6", false);

    let unasked = map_json(&tmp.join("without"), &video, false);
    assert!(
        unasked["scan"].is_null(),
        "without --scan the field must be null -- 'nobody looked' and 'looked and \
         found nothing' cannot look the same"
    );

    let asked = map_json(&tmp.join("with"), &video, true);
    assert!(
        asked["scan"].is_object(),
        "with --scan the field must be a summary even when the frames are clean"
    );
    let scan = &asked["scan"];
    assert!(scan["engine"].is_object());
    assert!(scan["detections_total"].is_number());
    // Whether an engine exists is a property of the machine; that the counts
    // are present and honest is a property of the code, and that is what this
    // pins.
    assert!(scan["unlocated_total"].is_number());
    assert!(scan["unreadable_frames"].is_number());
}

fn map_json(out_dir: &Path, video: &Path, scan: bool) -> serde_json::Value {
    let mut cmd = Command::new(BIN);
    cmd.arg("map")
        .arg(video)
        .arg("--skip-transcript")
        .arg("--out")
        .arg(out_dir);
    if scan {
        cmd.arg("--scan");
    }
    let out = cmd.output().expect("run map");
    assert!(
        out.status.success(),
        "map failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("map must emit JSON")
}

/// Reads a pixel out of a PNG, so a test can check that black really is there.
fn pixel(path: &Path, x: u32, y: u32) -> (u8, u8, u8) {
    let decoder = png::Decoder::new(std::fs::File::open(path).expect("open png"));
    let mut reader = decoder.read_info().expect("png header");
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).expect("png data");
    let channels = info.color_type.samples();
    let i = (y as usize * info.line_size) + (x as usize * channels);
    (buf[i], buf[i + 1], buf[i + 2])
}

#[test]
fn an_explicit_box_is_painted_and_the_source_is_left_alone() {
    // No OCR anywhere in this one, so it runs on every platform the core
    // builds for -- including the Linux job, which is the only place the
    // non-Windows paths get exercised at all.
    let Some(ff) = ffmpeg() else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let tmp = TempDir::new("redact-box");
    let src = tmp.join("frame.png");
    run_ffmpeg(
        &ff,
        &[
            "-y".as_ref(),
            "-hide_banner".as_ref(),
            "-loglevel".as_ref(),
            "error".as_ref(),
            "-f".as_ref(),
            "lavfi".as_ref(),
            "-i".as_ref(),
            "color=c=white:s=200x100".as_ref(),
            "-frames:v".as_ref(),
            "1".as_ref(),
            src.as_os_str(),
        ],
    );
    let before = std::fs::read(&src).expect("read source");

    let dest = tmp.join("out.png");
    let out = Command::new(BIN)
        .args(["redact"])
        .arg(&src)
        .args(["--box", "20,30,40,20", "--out"])
        .arg(&dest)
        .output()
        .expect("run redact");
    assert!(
        out.status.success(),
        "redact failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let json: serde_json::Value = serde_json::from_slice(&out.stdout).expect("redact emits JSON");
    assert_eq!(json["masks_applied"], 1);
    assert_eq!(json["frame"]["width"], 200);
    assert!(
        json["from_scan"].is_null(),
        "no scan was asked for, so nothing may claim one happened"
    );

    // Core's own read-back agrees, and this runs with no OCR engine anywhere
    // -- the pixel check is deterministic, so the Linux job verifies it too.
    assert_eq!(json["verification"]["boxes_black"], true);

    // Inside the box is black; a pixel just outside it is untouched.
    assert_eq!(pixel(&dest, 30, 40), (0, 0, 0), "the box was not painted");
    let (r, g, b) = pixel(&dest, 5, 5);
    assert!(
        r > 200 && g > 200 && b > 200,
        "paint escaped the box: {r},{g},{b}"
    );

    assert_eq!(
        before,
        std::fs::read(&src).unwrap(),
        "the source frame must come out byte-identical -- the review screen shows it"
    );
}

#[test]
fn a_box_outside_the_frame_is_refused_rather_than_silently_covering_nothing() {
    let Some(ff) = ffmpeg() else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let tmp = TempDir::new("redact-outside");
    let src = tmp.join("frame.png");
    run_ffmpeg(
        &ff,
        &[
            "-y".as_ref(),
            "-hide_banner".as_ref(),
            "-loglevel".as_ref(),
            "error".as_ref(),
            "-f".as_ref(),
            "lavfi".as_ref(),
            "-i".as_ref(),
            "color=c=white:s=200x100".as_ref(),
            "-frames:v".as_ref(),
            "1".as_ref(),
            src.as_os_str(),
        ],
    );

    let out = Command::new(BIN)
        .args(["redact"])
        .arg(&src)
        .args(["--box", "900,10,50,50", "--out"])
        .arg(tmp.join("out.png"))
        .output()
        .expect("run redact");

    // ffmpeg's drawbox would clip this to nothing and exit 0. Success here
    // would mean reporting a redaction that never touched a pixel.
    assert!(
        !out.status.success(),
        "an off-frame box must not report success"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("outside"), "the reason must be legible: {err}");
}

#[test]
fn what_the_scanner_found_is_gone_from_the_redacted_copy() {
    // The round trip, and the only assertion here that can fail for a real
    // reason: redact the fixture, then read the result back with the engine.
    let out = Command::new(BIN)
        .arg("redact")
        .arg(one_key_frame())
        .arg("--scan")
        .arg("--out")
        .arg(TempDir::new("redact-scan").join("out.png"))
        .output()
        .expect("run redact");

    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        if err.contains("no OCR engine") || err.contains("Windows-only") {
            eprintln!("{NO_ENGINE}");
            return;
        }
        panic!("redact failed: {err}");
    }

    let json: serde_json::Value = serde_json::from_slice(&out.stdout).expect("redact emits JSON");
    assert_eq!(
        json["from_scan"]["detections"], 1,
        "the fixture carries one key"
    );
    assert_eq!(json["from_scan"]["unmaskable"], 0);
    assert!(json["masks_applied"].as_u64().unwrap() >= 1);

    assert_eq!(json["verification"]["boxes_black"], true);
    assert_eq!(json["verification"]["ran"], true);
    // Zero is meaningful HERE because --scan painted everything it found; in
    // the reviewed path a person may leave findings visible and this number
    // stops being a pass/fail -- see review.rs in the tray.
    assert_eq!(
        json["verification"]["still_detected"], 0,
        "the key the scanner found is still readable in the redacted copy"
    );
}

/// S5.8. A pattern the scanner cannot use safely is refused at the door, with
/// the reason, rather than accepted and quietly ignored.
///
/// The refusal matters more than it looks: `--pattern` is how the app hands a
/// person's own words to core, and a word that is silently dropped leaves them
/// believing something is being hidden while nothing looks for it.
#[test]
fn a_pattern_too_short_to_be_safe_is_refused_at_the_command_line() {
    let out = core(&[
        "scan".as_ref(),
        "any.png".as_ref(),
        "--pattern".as_ref(),
        "ab".as_ref(),
    ]);
    assert!(!out.status.success(), "a two-letter pattern was accepted");
    let msg = stderr(&out);
    assert!(
        msg.contains("at least 4"),
        "got:
{msg}"
    );

    // And a bare flag is a typo, not a silent no-op.
    let bare = core(&["scan".as_ref(), "any.png".as_ref(), "--pattern".as_ref()]);
    assert!(!bare.status.success());
    assert!(
        stderr(&bare).contains("needs a word"),
        "got:
{}",
        stderr(&bare)
    );
}
