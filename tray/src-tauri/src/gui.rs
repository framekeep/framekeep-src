//! `framekeep-tray` -- the app. This is S3.1: a tray icon, a window that opens
//! and closes, and the same IPC server the headless binary runs.
//!
//! The window is deliberately almost empty. Its real screens are S4 and S5
//! work; today it says only what is true -- the app is running, the tray has
//! it -- with strings from `_design_system/copy.md` and colours from
//! `tokens.md`. Nothing in it promises paste or review, because neither
//! exists yet.
//!
//! Behaviour, all of it Windows-convention:
//!   - close button hides to the tray; the app keeps running ("Minimize to
//!     tray" is the settings default)
//!   - left-click on the tray icon shows the window; right-click, the menu
//!   - launching a second copy focuses the first instead of erroring
//!   - Quit actually quits, and takes the published address file with it

#![cfg_attr(windows, windows_subsystem = "windows")]

use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

use framekeep_tray::clipboard::Gesture;
use framekeep_tray::{retention::Retention, strings};

fn main() {
    tauri::Builder::default()
        // Registered before anything else touches shared state, and the order
        // is load-bearing. A second launch has to be recognised and turned
        // into "focus the first window" HERE -- the first version brought the
        // IPC server up before the builder ran, so instance two hit the
        // already-owned pipe and died at an error box instead of ever reaching
        // this plugin. Caught by launching a second copy for real.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_window(app);
        }))
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            queue_snapshot,
            paste_from_window,
            ingest_dropped,
            remove_recording,
            review_data,
            save_review,
            approve_recording,
            get_settings,
            set_theme,
            set_retention,
            set_minimize_to_tray,
            set_start_on_login,
            list_models,
            download_model,
            model_progress,
            connect_command,
            copy_connect_command,
            connect_client,
            connect_ready,
            app_version,
            shortcut_status,
            diagnostics,
            set_transcription,
            pick_models_dir,
            add_pattern,
            remove_pattern,
            watch_folder_on,
            watch_folder_off,
            clear_all_data,
            reset_settings,
            copy_prompt
        ])
        .setup(|app| {
            // The IPC server comes up before the window paints: an adapter
            // that connects during startup should find a working queue.
            let daemon = match framekeep_tray::bring_up(None) {
                Ok(d) => d,
                Err(message) => {
                    // Reached when the headless trayd owns the pipe -- a dev
                    // machine case; duplicate GUI launches never get here. A
                    // GUI process has no stderr anyone reads, so: message box.
                    alert(&message);
                    std::process::exit(1);
                }
            };
            for line in &daemon.report {
                eprintln!("{line}");
            }

            // The commands above read this instead of re-deriving it, so the
            // window and the pipeline share one set of retention rules.
            app.manage(daemon.retention.clone());

            // Folder watching, if the user asked for it. Off is the default
            // and the common case; `start_watching` returns None then and no
            // thread exists at all. In a mutex because the Settings screen
            // starts and stops it live -- dropping the handle stops watching.
            let watching = framekeep_tray::start_watching(
                &daemon.settings,
                &daemon.retention,
                queue_changed_notifier(app.handle()),
            );
            if watching.is_some() {
                eprintln!("Watching a folder for new recordings.");
            }
            app.manage(LiveSettings {
                settings: std::sync::Mutex::new(daemon.settings.clone()),
                watcher: std::sync::Mutex::new(watching),
            });

            // Pick up where a previous run stopped. The pipeline is kicked at
            // paste time, so without this, quitting mid-transcription leaves a
            // row saying "Extracting frames" forever -- a stall that looks
            // exactly like work. Found by killing the app mid-run for real.
            if let Ok(queue) = framekeep_tray::queue::Queue::open() {
                if let Ok(rows) = queue.list(200) {
                    for row in rows {
                        if matches!(
                            row.status,
                            framekeep_tray::queue::Status::ExtractingFrames
                                | framekeep_tray::queue::Status::Transcribing
                                | framekeep_tray::queue::Status::Scanning
                        ) {
                            let notify = queue_changed_notifier(app.handle());
                            framekeep_tray::pipeline::kick(row.handle, move |h| notify(h));
                        }
                    }
                }
            }

            // The shortcut is the second of exactly two ways the clipboard is
            // ever read. Registered here, next to the other one, so the whole
            // list is visible in one screenful -- see `clipboard.rs`.
            //
            // Whether it took is managed state, because the Shortcuts screen
            // has to be able to say so: registration can fail, and the failure
            // is invisible from inside the app otherwise.
            let shortcut = register_paste_shortcut(app.handle(), daemon.retention.clone());
            if let Some(detail) = &shortcut.detail {
                eprintln!("{detail}");
            }
            app.manage(shortcut);

            let retention = daemon.retention.clone();
            let listener = daemon.listener;
            // Keeps the address file alive exactly as long as the server
            // thread. On Quit the process exits without running this thread's
            // destructors; the RunEvent::Exit handler below removes the file
            // explicitly instead.
            let published = daemon.published;
            std::thread::spawn(move || {
                let _published = published;
                let retention = retention.clone();
                // If the accept loop ever dies the window still works; the
                // adapter falls back to standalone, a state it already handles.
                let _ = framekeep_tray::serve(&listener, move || {
                    framekeep_tray::connection_handlers(&retention)
                });
            });

            let open = MenuItem::with_id(app, "open", strings::TRAY_OPEN, true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", strings::TRAY_QUIT, true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &quit])?;

            TrayIconBuilder::with_id("framekeep")
                .icon(app.default_window_icon().expect("icon compiled in").clone())
                .tooltip(strings::TRAY_TOOLTIP)
                .menu(&menu)
                // Left click is "open", not "menu" -- the menu stays on right
                // click, which is where Windows users look for it.
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "open" => show_window(app),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        show_window(tray.app_handle());
                    }
                })
                .build(app)?;
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing hides. The app's whole job is to sit in the tray waiting
            // for a paste; a close button that killed it would also kill the
            // MCP adapter's bridge mid-conversation.
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                // The setting decides, and the default keeps the old behaviour.
                // Read live rather than captured at startup: somebody who turns
                // this off expects the very next close to obey it.
                let hide = window
                    .try_state::<LiveSettings>()
                    .map(|s| s.settings.lock().unwrap().app.minimize_to_tray)
                    .unwrap_or(true);
                if hide {
                    let _ = window.hide();
                    api.prevent_close();
                } else {
                    // Closing means closing. The tray icon goes with it, and so
                    // does the IPC server -- which is the honest consequence,
                    // not a bug: an adapter that finds nothing listening falls
                    // back to standalone and says so.
                    window.app_handle().exit(0);
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("the window system refused to start")
        .run(|_app, event| {
            if let tauri::RunEvent::Exit = event {
                // process::exit skips destructors, so the Published guard in
                // the server thread never drops. Take the address file down by
                // hand: a stale one only costs the adapter an instant refusal,
                // but clean is clean.
                if let Some(path) = framekeep_tray::transport::address_file() {
                    let _ = std::fs::remove_file(path);
                }
            }
        });
}

/// `Ctrl + Shift + V` -- paste a recording without finding the window first.
///
/// Two traps, both from research 4.3 and both avoided here:
///   - the handler fires for **Pressed and Released**, so acting on every
///     event would ingest the same clipboard twice per keypress
///   - plain `Ctrl + V` is not ours to take; it belongs to whatever the person
///     is typing in
fn register_paste_shortcut(app: &AppHandle, retention: Retention) -> ShortcutStatus {
    let shortcut = Shortcut::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::KeyV);

    let plugin = tauri_plugin_global_shortcut::Builder::new()
        .with_handler(move |app, _pressed, event| {
            if event.state() != ShortcutState::Pressed {
                return;
            }
            let result = framekeep_tray::paste_now(
                &Gesture::global_shortcut(),
                &retention,
                queue_changed_notifier(app),
            );
            report(app, result);
        })
        .build();

    if let Err(e) = app.plugin(plugin) {
        return ShortcutStatus::unavailable(format!(
            "Framekeep couldn't set up the shortcut ({e}). Pasting in the window still works."
        ));
    }
    if let Err(e) = app.global_shortcut().register(shortcut) {
        // Another program may already own it. Not fatal, and not worth a
        // dialog: the window's own paste is the primary path. It is worth
        // *saying*, though -- see `ShortcutStatus`.
        return ShortcutStatus::unavailable(format!(
            "Another program is using {}. Paste in the window instead ({e}).",
            ShortcutStatus::CHORD
        ));
    }
    ShortcutStatus::registered()
}

/// Whether `Ctrl + Shift + V` actually reached the OS, in a form the window
/// can show.
///
/// Both failures above used to end at `eprintln!`, and a GUI process on Windows
/// has no stderr anyone reads -- in the packaged build there is not even a
/// console to attach to. So the one shortcut that works while the window is
/// hidden could be silently dead, and the app's only account of itself was a
/// line printed into nowhere. That is the same shape as the four worst bugs
/// this project has had: silent, and indistinguishable from success.
///
/// The Shortcuts screen reads this. A list of chords that cannot say which of
/// them are live is a picture of a feature, which is what the Settings screen
/// spent 18/08 getting rid of.
#[derive(Clone, serde::Serialize)]
struct ShortcutStatus {
    /// One spelling of the chord, shared by this file and the screen. Written
    /// the way Windows writes it.
    chord: &'static str,
    registered: bool,
    /// Why not, when not. Always a sentence a person can act on.
    detail: Option<String>,
}

impl ShortcutStatus {
    const CHORD: &'static str = "Ctrl + Shift + V";

    fn registered() -> Self {
        ShortcutStatus {
            chord: Self::CHORD,
            registered: true,
            detail: None,
        }
    }

    fn unavailable(detail: String) -> Self {
        ShortcutStatus {
            chord: Self::CHORD,
            registered: false,
            detail: Some(detail),
        }
    }
}

/// The pipeline calls this after every stage it completes; the window listens
/// and re-reads the queue. The payload is only the handle -- whoever hears it
/// asks the queue, so there is exactly one description of a row.
fn queue_changed_notifier(app: &AppHandle) -> std::sync::Arc<dyn Fn(&str) + Send + Sync> {
    let app = app.clone();
    std::sync::Arc::new(move |handle: &str| {
        let _ = app.emit("queue-changed", handle.to_string());
    })
}

/// Show the person what their paste did, and bring the window up to say it.
fn report(app: &AppHandle, result: Result<framekeep_tray::PasteResult, String>) {
    let message = match result {
        Ok(r) => r.message,
        Err(e) => e,
    };
    show_window(app);
    // The window renders it. Emitting rather than blocking on a dialog: a
    // message box would steal focus from whatever they were recording.
    let _ = app.emit("paste-result", message);
    let _ = app.emit("queue-changed", "");
}

// --- commands: the window's only doors into the app ------------------------
//
// Three, and the shape of each is a rule:
//   - the snapshot goes through QueueHandlers::list, the same door the IPC
//     surface uses, so the GUI and a client cannot disagree about a row
//   - paste requires being *called*, which requires a person's keypress in the
//     window -- the webview has no clipboard access of its own (and must never
//     get any; see clipboard.rs)
//   - remove is Queue::purge, the single deletion path (retention L4)

#[tauri::command]
fn queue_snapshot(state: tauri::State<'_, Retention>) -> Result<serde_json::Value, String> {
    let queue = framekeep_tray::queue::Queue::open().map_err(|e| e.to_string())?;
    let handlers = framekeep_tray::handlers::QueueHandlers::new(queue, state.inner().clone());
    handlers
        .list(&serde_json::json!({ "limit": 200 }))
        .map_err(|(_, message)| message)
}

#[tauri::command]
fn paste_from_window(app: AppHandle, state: tauri::State<'_, Retention>) -> Result<String, String> {
    let result = framekeep_tray::paste_now(
        &Gesture::paste_in_window(),
        state.inner(),
        queue_changed_notifier(&app),
    )?;
    let _ = app.emit("queue-changed", "");
    Ok(result.message)
}

/// Files dropped on the window. The OS hands Tauri real paths; they go through
/// the same decision table as a paste (`paste::decide_files`), so a format the
/// app accepts one way it accepts both ways, and the refusals share sentences.
#[tauri::command]
fn ingest_dropped(
    app: AppHandle,
    state: tauri::State<'_, Retention>,
    paths: Vec<String>,
) -> Result<String, String> {
    let paths: Vec<std::path::PathBuf> = paths.into_iter().map(Into::into).collect();
    let outcome = framekeep_tray::paste::decide_files(&paths, &|p| p.exists());
    let result =
        framekeep_tray::ingest_outcome(outcome, state.inner(), queue_changed_notifier(&app))?;
    let _ = app.emit("queue-changed", "");
    Ok(result.message)
}

#[tauri::command]
fn remove_recording(
    app: AppHandle,
    state: tauri::State<'_, Retention>,
    handle: String,
) -> Result<(), String> {
    let queue = framekeep_tray::queue::Queue::open().map_err(|e| e.to_string())?;
    queue
        .purge(&handle, state.inner())
        .map_err(|e| e.to_string())?;
    let _ = app.emit("queue-changed", handle);
    Ok(())
}

// --- review commands: S5.4 -------------------------------------------------
//
// The same shape as the queue trio: thin doors over library code, so the
// behaviour lives where the tests are. Three properties matter here:
//   - the window only ever receives masked values, because core only ever
//     emitted masked values -- there is no raw secret to leak on this side of
//     the process boundary
//   - ticks go through review::save_ticks, which shape-checks them against the
//     scan; a stale window cannot mislabel a finding
//   - approve runs review::apply, the ONE path out of needs_review, and it
//     paints before it flips

/// Everything the review screen renders for one recording.
#[tauri::command]
fn review_data(handle: String) -> Result<serde_json::Value, String> {
    let queue = framekeep_tray::queue::Queue::open().map_err(|e| e.to_string())?;
    let row = queue
        .get(&handle)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "That recording is no longer in the queue.".to_string())?;

    let cache = queue.cache_dir(&handle);
    let scan = framekeep_tray::review::load_scan(&cache)?;
    let (ticks, extras) = framekeep_tray::review::load_decisions(&cache, &scan);

    let frames: Vec<serde_json::Value> = scan
        .frames
        .iter()
        .zip(&ticks)
        .map(|(frame, frame_ticks)| {
            serde_json::json!({
                "file": frame.file,
                "pts_time": frame.pts_time,
                "error": frame.error,
                "detections": frame
                    .detections
                    .iter()
                    .zip(frame_ticks)
                    .map(|(d, &approved)| serde_json::json!({
                        "kind": d.kind,
                        "masked": d.masked,
                        "boxes": d.boxes,
                        "located": d.located,
                        "approved": approved,
                    }))
                    .collect::<Vec<_>>(),
            })
        })
        .collect();

    Ok(serde_json::json!({
        "handle": row.handle,
        "name": row.display_name,
        "stage": row.status.as_str(),
        "duration_ms": row.duration_ms,
        "width": row.width,
        "height": row.height,
        "frame_count": row.frame_count,
        "engine": scan.engine,
        "unreadable_frames": scan.unreadable_frames,
        "frames": frames,
        // The regions the person drew, per frame -- so reopening a review
        // shows their own work back to them.
        "extras": extras,
    }))
}

/// Persist the decisions -- `Save & keep reviewing`. The row stays in review.
#[tauri::command]
fn save_review(
    handle: String,
    ticks: Vec<Vec<bool>>,
    extras: Vec<Vec<framekeep_tray::review::BoxF>>,
) -> Result<(), String> {
    let queue = framekeep_tray::queue::Queue::open().map_err(|e| e.to_string())?;
    let cache = queue.cache_dir(&handle);
    let scan = framekeep_tray::review::load_scan(&cache)?;
    framekeep_tray::review::save_decisions(&cache, &scan, &ticks, &extras)
}

/// `Send to chat`: paint what is ticked, verify the paint, flip the row.
///
/// Async because the painting is real work (~100ms a frame through ffmpeg);
/// spawn_blocking keeps it off the runtime the window's events ride on.
#[tauri::command]
async fn approve_recording(
    app: AppHandle,
    handle: String,
    ticks: Vec<Vec<bool>>,
    extras: Vec<Vec<framekeep_tray::review::BoxF>>,
) -> Result<serde_json::Value, String> {
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let queue = framekeep_tray::queue::Queue::open().map_err(|e| e.to_string())?;
        let cache = queue.cache_dir(&handle);
        let scan = framekeep_tray::review::load_scan(&cache)?;
        // The decisions the person is looking at, not whatever an earlier
        // save left behind: the window sends its state with the click.
        framekeep_tray::review::save_decisions(&cache, &scan, &ticks, &extras)?;
        let runner = framekeep_tray::pipeline::CoreBinary::locate()?;
        let applied = framekeep_tray::review::apply(&queue, &handle, &runner)?;
        Ok::<_, String>((handle, applied))
    })
    .await
    .map_err(|e| format!("The approval thread died ({e}). Please report this."))?;

    let (handle, applied) = outcome?;
    let _ = app.emit("queue-changed", handle);
    serde_json::to_value(applied).map_err(|e| e.to_string())
}

// --- settings commands: the Settings screen's doors -------------------------
//
// One live copy of the settings, guarded, plus the watcher handle it may start
// and stop. Every mutation goes disk-first: settings::save, THEN the in-memory
// copy -- a save that fails must not leave the screen showing a state the next
// launch will not have.
//
// Deliberately narrow: retention clones captured at startup (the IPC serve
// loop, the global shortcut) keep their values until the next launch. What
// that costs is cosmetic -- a countdown label served over the pipe may lag a
// keep-days change by one restart. The GUI itself reads the live copy.

struct LiveSettings {
    settings: std::sync::Mutex<framekeep_tray::settings::Settings>,
    watcher: std::sync::Mutex<Option<framekeep_tray::watcher::Handle>>,
}

impl LiveSettings {
    /// Persist first, remember second.
    fn commit(
        &self,
        change: impl FnOnce(&mut framekeep_tray::settings::Settings),
    ) -> Result<framekeep_tray::settings::Settings, String> {
        let mut guard = self.settings.lock().unwrap();
        let mut next = guard.clone();
        change(&mut next);
        framekeep_tray::settings::save(&next)
            .map_err(|e| format!("Couldn't save your settings ({e}). Nothing was changed."))?;
        *guard = next.clone();
        Ok(next)
    }
}

#[tauri::command]
fn get_settings(state: tauri::State<'_, LiveSettings>) -> framekeep_tray::settings::Settings {
    state.settings.lock().unwrap().clone()
}

#[tauri::command]
fn set_theme(
    state: tauri::State<'_, LiveSettings>,
    theme: framekeep_tray::settings::Theme,
) -> Result<(), String> {
    state.commit(|s| s.theme = theme).map(|_| ())
}

/// The auto-delete toggle and the keep-days choice -- and answering is the
/// one-time question of retention L5b: the first change a person makes here
/// IS their answer, so `choice_made` flips with it.
#[tauri::command]
fn set_retention(
    state: tauri::State<'_, LiveSettings>,
    delete: bool,
    keep_days: u64,
) -> Result<(), String> {
    let keep_days = keep_days.clamp(1, 90);
    state
        .commit(|s| {
            s.retention.delete_copied_sources = delete;
            s.retention.keep_days = keep_days;
            s.retention.choice_made = true;
        })
        .map(|_| ())
}

/// Add a word the scanner should always hide (S5.8).
///
/// Validated *here*, before it is saved, and that placement is the point. A
/// pattern core would reject arrives as `--pattern ab` on the next scan, core
/// answers with a usage error, and `run_stages` turns the whole recording red
/// -- one typo in Settings would break importing. So nothing unusable is ever
/// written down.
///
/// The rule itself lives in `settings::check_pattern`, where it is tested;
/// this only decides what to do with the answer.
#[tauri::command]
fn add_pattern(
    state: tauri::State<'_, LiveSettings>,
    word: String,
) -> Result<framekeep_tray::settings::Settings, String> {
    use framekeep_tray::settings::{check_pattern, MAX_PATTERNS};

    let word = check_pattern(&word)?;
    {
        let existing = &state.settings.lock().unwrap().redaction.patterns;
        if existing.iter().any(|p| p.eq_ignore_ascii_case(&word)) {
            return Err(format!("{word} is already on the list."));
        }
        if existing.len() >= MAX_PATTERNS {
            return Err(format!(
                "That's {MAX_PATTERNS} patterns, the most Framekeep keeps. Remove one to add another."
            ));
        }
    }
    state.commit(|s| s.redaction.patterns.push(word))
}

#[tauri::command]
fn remove_pattern(
    state: tauri::State<'_, LiveSettings>,
    word: String,
) -> Result<framekeep_tray::settings::Settings, String> {
    state.commit(|s| s.redaction.patterns.retain(|p| p != &word))
}

/// `Minimize to tray`. Real now: `CloseRequested` reads it live, so the very
/// next close obeys the switch rather than the value captured at startup.
#[tauri::command]
fn set_minimize_to_tray(
    state: tauri::State<'_, LiveSettings>,
    on: bool,
) -> Result<framekeep_tray::settings::Settings, String> {
    state.commit(|s| s.app.minimize_to_tray = on)
}

/// `Start Framekeep on system startup`.
///
/// Windows owns this, not us. A packaged app declares a `startupTask` in its
/// manifest and then asks the system to enable or disable it; the user can
/// override that in Task Manager and never tell the app, which is why the
/// answer is read back from the system rather than trusted from our own file.
///
/// Outside a package there is no startup task to toggle, so the switch says so
/// instead of writing a registry key behind the user's back -- a Run key
/// written by a Store app is exactly the kind of thing certification asks
/// about, and it would survive uninstalling.
#[tauri::command]
fn set_start_on_login(
    state: tauri::State<'_, LiveSettings>,
    on: bool,
) -> Result<framekeep_tray::settings::Settings, String> {
    framekeep_tray::startup::set(on)?;
    let actual = framekeep_tray::startup::enabled();
    state.commit(|s| s.app.start_on_login = actual)
}

/// Puts a ready-to-paste line for the person's AI client on the clipboard.
///
/// The step nobody was told about. Approving a recording does not send it
/// anywhere -- it unlocks it, and the frames travel only when a model asks
/// through `video_frames`. Which means the person has to go to their AI and
/// name the file, and until now nothing in the app said so or gave them the
/// path to name.
///
/// Text, not a file: the client is usually a terminal or an editor pane, and
/// pasting a video into one of those was never the workflow. What the model
/// needs is the path -- it has the tools to open it.
#[cfg_attr(feature = "gui", tauri::command)]
fn copy_prompt(handle: String) -> Result<String, String> {
    let queue = framekeep_tray::queue::Queue::open().map_err(|e| e.to_string())?;
    let row = queue
        .get(&handle)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "That recording is no longer in the queue.".to_string())?;

    let line = framekeep_tray::strings::ai_prompt(&row.source_path.to_string_lossy());
    framekeep_tray::clipboard::write_text(&line).map_err(|e| e.to_string())?;
    Ok(line)
}

/// The connect command, for the Setup screen to show.
#[tauri::command]
fn connect_command() -> &'static str {
    framekeep_tray::strings::CONNECT_COMMAND
}

/// The same line, on the clipboard.
///
/// A command of its own rather than a general "copy this text": the window
/// asks for a named thing, and `clipboard.rs` stays the only file that can put
/// anything anywhere. Handing the webview an arbitrary-text door would widen
/// principle IV's one door for the sake of saving a function.
#[tauri::command]
fn copy_connect_command() -> Result<String, String> {
    let line = framekeep_tray::strings::CONNECT_COMMAND;
    framekeep_tray::clipboard::write_text(line).map_err(|e| e.to_string())?;
    Ok(line.to_string())
}

/// The app's own version, for the sidebar and the About section.
///
/// Read from the package rather than written into the window: the string was
/// typed into `index.html` as `v0.1.0`, and a version that has to be
/// remembered in two places is a version that will disagree with itself at the
/// first release. This is the same rule the ffmpeg line follows one card over.
#[tauri::command]
fn app_version(app: AppHandle) -> String {
    app.package_info().version.to_string()
}

/// What the Shortcuts screen shows next to the global chord.
#[tauri::command]
fn shortcut_status(state: tauri::State<'_, ShortcutStatus>) -> ShortcutStatus {
    (*state).clone()
}

/// Fetch a speech model, because until now nothing in the app could.
///
/// Settings was ruled a reporting surface in S5.9 -- no knobs -- and that
/// ruling still stands: the three it turned down were behaviour that already
/// ran, or constants somebody measured. This is neither. It is the one action
/// the product needs and had no door for, and the clean-machine run of 21/08
/// is what made that concrete: a Store install could never transcribe anything
/// without opening a terminal, while the listing promised Whisper and copy.md
/// told people to "download it in Settings" -- a sentence pointing at nothing.
///
/// A person clicks it, so the promise on the Privacy screen survives word for
/// word: one download, and only when you ask. Nothing here downloads on its
/// own, and the paste path still degrades to frames-only rather than nagging.
///
/// Long-running by nature -- half a gigabyte -- so it is async and the window
/// polls `model_progress` while it waits. The error, when there is one, is
/// core's own sentence: it names the checksum, the short read, or the network.
#[tauri::command]
async fn download_model(name: String) -> Result<serde_json::Value, String> {
    use framekeep_tray::pipeline::{CoreBinary, Runner};

    // Blocking work off the UI thread. `--yes` is what turns core's preview
    // into a fetch, and it is passed here rather than made a default there:
    // the confirmation belongs to whoever is asking, and on the command line
    // that is still a person typing it.
    let models = tauri::async_runtime::spawn_blocking(move || {
        let core = CoreBinary::locate()?;
        core.run(&["models".into(), "get".into(), name, "--yes".into()])?;
        core.run(&["models".into(), "--json".into()])
    })
    .await
    .map_err(|e| format!("The download task ended unexpectedly: {e}"))??;

    serde_json::from_str(&models).map_err(|e| format!("core answered something unreadable ({e})."))
}

/// How far that download has got, measured off the disk rather than reported.
///
/// core writes to a `.partial` neighbour and renames only once the checksum
/// matches, so the size of that file is the download's own ground truth. No
/// progress protocol to keep in sync between two binaries, and no number that
/// can claim progress the bytes do not have -- which is the failure mode this
/// project has hit often enough to prefer measuring.
///
/// Returns 0 when nothing is in flight, which is also what a finished download
/// looks like the instant before the await returns. The window treats both the
/// same, so neither needs a state machine.
#[tauri::command]
fn model_progress() -> u64 {
    let Some(dir) = framekeep_tray::settings::load()
        .0
        .transcription
        .models_dir
        .or_else(|| {
            std::env::var_os("USERPROFILE")
                .or_else(|| std::env::var_os("HOME"))
                .map(|home| {
                    std::path::PathBuf::from(home)
                        .join(".framekeep")
                        .join("models")
                })
        })
    else {
        return 0;
    };
    // Any `.partial` in there: only one download runs at a time, and looking
    // for the largest avoids having to rebuild core's filename rule here --
    // a second copy of that rule is a second thing to get wrong.
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "partial"))
        .filter_map(|e| e.metadata().ok())
        .map(|m| m.len())
        .max()
        .unwrap_or(0)
}

/// Whether the one-click path can work at all, asked before it is offered.
///
/// The screen needs this to decide what to show, not just what to say after a
/// failure: a button that is always there and always fails is worse than a row
/// that says up front what is missing. Two separate answers because they have
/// different fixes -- install Node, versus reinstall the app.
#[derive(serde::Serialize)]
struct ConnectReady {
    /// A Node was found on PATH. Its path, so the screen can name it.
    node: Option<String>,
    /// The app is carrying the adapter. False in a dev tree with no build.
    adapter: bool,
}

#[tauri::command]
fn connect_ready() -> ConnectReady {
    ConnectReady {
        node: framekeep_tray::adapter::node().map(|p| p.display().to_string()),
        adapter: framekeep_tray::adapter::locate().is_ok(),
    }
}

/// Write the MCP config into a folder the person picks. S6.6.
///
/// The folder comes from the system picker and only from there, the same rule
/// `watch_folder_on` follows: the webview never holds a path of its own. Here
/// it is also what makes the command correct rather than merely tidy -- `init`
/// resolves the nearest repository from where it starts, so a path invented
/// anywhere else would write three config files somewhere no client reads and
/// report success for all three.
#[tauri::command]
async fn connect_client(
    app: AppHandle,
) -> Result<Option<framekeep_tray::adapter::Connected>, String> {
    use tauri_plugin_dialog::DialogExt;

    let picked = app.dialog().file().blocking_pick_folder();
    let Some(folder) = picked.and_then(|f| f.into_path().ok()) else {
        return Ok(None); // Closed the picker. Not an error, and not a change.
    };
    framekeep_tray::adapter::connect(&folder).map(Some)
}

/// What this machine actually has, straight from core: which ffmpeg will run,
/// which version it reports, whether speech is available, where the cache
/// lives and how long it stays.
///
/// Asked every time the screen opens rather than cached for the session. The
/// answers move while the app is running -- a model finishes downloading, the
/// download folder is changed two cards up on the same screen -- and a cached
/// diagnostic is a screen that describes the app as it was at launch. Cost is
/// one short-lived process; the honest answer is worth it.
#[tauri::command]
fn diagnostics() -> Result<serde_json::Value, String> {
    use framekeep_tray::pipeline::{CoreBinary, Runner};
    let core = CoreBinary::locate()?;
    let out = core.run(&["doctor".into(), "--json".into()])?;
    serde_json::from_str(&out).map_err(|e| format!("core answered something unreadable ({e})."))
}

/// The speech model catalogue, straight from core.
///
/// Asked rather than duplicated: core owns the files and the sizes, and a
/// second list in the window would be wrong the day core gained an entry.
/// Returns the raw JSON so a new field reaches the screen without a change
/// here as well.
#[tauri::command]
fn list_models() -> Result<serde_json::Value, String> {
    use framekeep_tray::pipeline::{CoreBinary, Runner};
    let core = CoreBinary::locate()?;
    let out = core.run(&["models".into(), "--json".into()])?;
    serde_json::from_str(&out).map_err(|e| format!("core answered something unreadable ({e})."))
}

/// `Transcription model` and `Language`.
///
/// Both are `None` for "let core decide", which is a real answer rather than
/// a missing one: core picks the model that fits this machine's RAM, and
/// whisper detects the language better than a guess would.
#[tauri::command]
fn set_transcription(
    state: tauri::State<'_, LiveSettings>,
    model: Option<String>,
    language: Option<String>,
) -> Result<framekeep_tray::settings::Settings, String> {
    state.commit(|s| {
        s.transcription.model = model.filter(|m| m != "auto");
        s.transcription.language = language.filter(|l| l != "auto");
    })
}

/// `Model download location`. Picker only, like the watched folder, and for
/// the same packaging reason -- see `settings::Watch`.
///
/// Passing `None` puts it back to core's default rather than storing a copy of
/// that path: a default written down is a default that goes stale.
#[tauri::command]
async fn pick_models_dir(
    app: AppHandle,
    state: tauri::State<'_, LiveSettings>,
) -> Result<Option<framekeep_tray::settings::Settings>, String> {
    use tauri_plugin_dialog::DialogExt;

    // The `reset` arm this used to carry is gone with the UI that reached it:
    // clicking an already-chosen path a second time wiped it back to the
    // default, unannounced and unconfirmed. Picking the default folder is the
    // way back, and it goes through the same dialog as every other choice.
    let picked = app.dialog().file().blocking_pick_folder();
    let Some(folder) = picked.and_then(|f| f.into_path().ok()) else {
        return Ok(None);
    };
    state
        .commit(|s| s.transcription.models_dir = Some(folder.clone()))
        .map(Some)
}

/// Turn folder watching on. The folder comes from the system picker and only
/// from there -- see `settings::Watch` for why that is a packaging constraint,
/// not a preference. Returns the settings, or None if they cancelled.
#[tauri::command]
async fn watch_folder_on(
    app: AppHandle,
    state: tauri::State<'_, LiveSettings>,
) -> Result<Option<framekeep_tray::settings::Settings>, String> {
    use tauri_plugin_dialog::DialogExt;

    let picked = app.dialog().file().blocking_pick_folder();
    let Some(folder) = picked.and_then(|f| f.into_path().ok()) else {
        return Ok(None); // They closed the picker. Not an error, not a change.
    };

    let next = state.commit(|s| {
        s.watch = Some(framekeep_tray::settings::Watch {
            folder: folder.clone(),
            // From now: two hundred old videos in that folder import nothing.
            since: framekeep_tray::queue::now_unix(),
        });
    })?;

    // Live swap: stop whatever ran before, start on the new folder.
    let retention = { next.retention() };
    let fresh = framekeep_tray::start_watching(&next, &retention, queue_changed_notifier(&app));
    let mut watcher = state.watcher.lock().unwrap();
    if let Some(old) = watcher.take() {
        old.stop();
    }
    *watcher = fresh;

    Ok(Some(next))
}

#[tauri::command]
fn watch_folder_off(
    state: tauri::State<'_, LiveSettings>,
) -> Result<framekeep_tray::settings::Settings, String> {
    let next = state.commit(|s| s.watch = None)?;
    if let Some(old) = state.watcher.lock().unwrap().take() {
        old.stop();
    }
    Ok(next)
}

/// Danger zone. Every row goes through `Queue::purge` -- the single deletion
/// path, so the retention verdicts still apply: files the user pointed at are
/// never touched, and models are not this button's business at all.
#[tauri::command]
fn clear_all_data(app: AppHandle, state: tauri::State<'_, Retention>) -> Result<String, String> {
    let queue = framekeep_tray::queue::Queue::open().map_err(|e| e.to_string())?;
    let rows = queue.list(200).map_err(|e| e.to_string())?;
    let total = rows.len();
    for row in rows {
        queue
            .purge(&row.handle, state.inner())
            .map_err(|e| e.to_string())?;
    }
    let _ = app.emit("queue-changed", "");
    Ok(format!(
        "Removed {total} recording{} and their frames.",
        if total == 1 { "" } else { "s" }
    ))
}

/// Back to defaults: the settings file is deleted -- the fresh-install state
/// is "no file", not "a file full of defaults" -- and the watcher stops.
#[tauri::command]
fn reset_settings(
    state: tauri::State<'_, LiveSettings>,
) -> Result<framekeep_tray::settings::Settings, String> {
    if let Some(path) = framekeep_tray::settings::path() {
        if let Err(e) = std::fs::remove_file(&path) {
            if e.kind() != std::io::ErrorKind::NotFound {
                return Err(format!("Couldn't reset ({e}). Nothing was changed."));
            }
        }
    }
    let defaults = framekeep_tray::settings::Settings::default();
    *state.settings.lock().unwrap() = defaults.clone();
    if let Some(old) = state.watcher.lock().unwrap().take() {
        old.stop();
    }
    Ok(defaults)
}

fn show_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

/// A startup failure a person can see. GUI processes on Windows have no
/// console, and a process that dies silently reads as "the app is broken".
#[cfg(windows)]
fn alert(message: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
    let wide = |s: &str| {
        s.encode_utf16()
            .chain(std::iter::once(0))
            .collect::<Vec<u16>>()
    };
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            wide(message).as_ptr(),
            wide(strings::TRAY_TOOLTIP).as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
}

#[cfg(not(windows))]
fn alert(message: &str) {
    eprintln!("{message}");
}
