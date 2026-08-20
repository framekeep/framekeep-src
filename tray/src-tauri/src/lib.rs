//! `framekeep-tray` -- the GUI, the queue, and the IPC server.
//!
//! One of three processes, and the only one with a face:
//!
//! ```text
//! framekeep-core    all video processing. No UI, no IPC server.
//! framekeep-tray    this. GUI + queue + IPC server. Calls core for the work.
//! framekeep-mcp     MCP adapter. Tries IPC first; falls back to core alone.
//! ```
//!
//! Nothing here knows how to handle video, and nothing here should learn. An
//! ffmpeg argument in this crate is in the wrong crate -- see `AGENTS.md`.
//!
//! The IPC server is separable from the window on purpose: everything except
//! the Tauri shell builds and tests on a machine with no display at all. The
//! GUI lives behind the `gui` feature so CI keeps that property.

pub mod clipboard;
pub mod handle;
pub mod handlers;
pub mod method;
pub mod paste;
pub mod pipeline;
pub mod protocol;
pub mod queue;
pub mod retention;
pub mod review;
pub mod session;
pub mod settings;
pub mod startup;
pub mod strings;
pub mod style_theme;
pub mod table_layout;
pub mod transport;
pub mod ui_contract;
pub mod watcher;

use session::{Handlers, Session};
use transport::Listener;

/// Everything a running server needs, brought up in the right order.
///
/// Shared by both binaries -- `framekeep-trayd` (headless) and `framekeep-tray`
/// (the app) -- because two copies of a startup sequence drift, and the drift
/// would be silent: one binary sweeping retention that the other forgot.
pub struct Daemon {
    pub listener: Listener,
    /// Held for the server's lifetime; dropping it takes the address file down.
    pub published: transport::Published,
    pub retention: retention::Retention,
    /// What the user has decided. Read once at startup; the window writes it
    /// back through `settings::save` when they change something.
    pub settings: settings::Settings,
    /// What startup did, one line each, for whoever is looking at output.
    pub report: Vec<String>,
}

/// Retention first, then bind, then publish. Fails only when there is nowhere
/// to listen -- a queue that cannot open is reported and worked around, because
/// an app that refuses to start over yesterday's database helps nobody.
pub fn bring_up(address: Option<&str>) -> Result<Daemon, String> {
    let mut report = Vec::new();

    if retention::default_recordings_dir().is_none() {
        return Err(
            "Couldn't work out where your home folder is, so there's nowhere to keep the queue."
                .to_string(),
        );
    }

    // The rules come from what the user decided, not from a hardcoded default.
    // Until this existed `choice_made` was pinned to false, so the one-time
    // question in retention L5b could be asked but never answered -- and
    // auto-delete could never run for anyone.
    let (settings, complaint) = settings::load();
    if let Some(said) = complaint {
        report.push(said);
    }
    let retention = settings.retention();

    // Before the first client can ask for anything. A policy that only runs on
    // a timer has not run on a machine that is opened once a week.
    match queue::Queue::open() {
        Ok(queue) => match handlers::run_retention(&queue, &retention, queue::now_unix()) {
            Ok(said) => report.push(said),
            Err(e) => report.push(e.to_string()),
        },
        Err(e) => report.push(e.to_string()),
    }

    let listener = match address {
        Some(name) => Listener::bind_at(name),
        None => Listener::bind(),
    }
    .map_err(|e| e.to_string())?;

    let published = transport::Published::write(listener.address()).unwrap_or_else(|e| {
        report.push(format!(
            "Couldn't tell the MCP adapter where to find this app ({e}). It will run standalone."
        ));
        transport::Published::none()
    });

    Ok(Daemon {
        listener,
        published,
        retention,
        settings,
        report,
    })
}

/// Start watching the folder the user chose, if they chose one.
///
/// `None` when watching is off, which is the default and the common case. The
/// returned handle stops the thread when dropped, so switching watching off
/// is a matter of dropping it.
pub fn start_watching(
    settings: &settings::Settings,
    retention: &retention::Retention,
    on_change: std::sync::Arc<dyn Fn(&str) + Send + Sync>,
) -> Option<watcher::Handle> {
    let watch = settings.watch.clone()?;
    let retention = retention.clone();
    Some(watcher::start(watch, move |path| {
        // Same door a paste goes through: ingest, then kick the pipeline. An
        // import that skipped either would be a second, subtly different way
        // for a recording to enter the system.
        let Ok(queue) = queue::Queue::open() else {
            return;
        };
        let mut handlers = handlers::QueueHandlers::new(queue, retention.clone());
        if let Ok(reply) = handlers.ingest(&serde_json::json!({ "path": path.to_string_lossy() })) {
            if let Some(handle) = reply["handle"].as_str() {
                if reply["already_queued"] == serde_json::Value::Bool(false) {
                    let notify = on_change.clone();
                    pipeline::kick(handle.to_string(), move |h| notify(h));
                }
            }
        }
    }))
}

/// The handlers every new connection gets: the queue, or an honest refusal
/// when it cannot be opened. One per connection -- separate SQLite handles,
/// which is also how it works between processes.
pub fn connection_handlers(retention: &retention::Retention) -> Box<dyn Handlers + Send> {
    match queue::Queue::open() {
        Ok(queue) => Box::new(handlers::QueueHandlers::new(queue, retention.clone())),
        Err(e) => Box::new(handlers::Unavailable(e.to_string())),
    }
}

/// Read the clipboard because a person asked, and queue whatever was on it.
///
/// The whole paste path in one function, so the sequence is readable in one
/// place: gesture -> clipboard -> decision -> queue. Returns the sentence to
/// show, which is a sentence in every case -- a paste that queued nothing is
/// still a paste that deserves an answer.
pub fn paste_now(
    gesture: &clipboard::Gesture,
    retention: &retention::Retention,
    on_change: std::sync::Arc<dyn Fn(&str) + Send + Sync>,
) -> Result<PasteResult, String> {
    let content = clipboard::read(gesture).map_err(|e| e.to_string())?;
    ingest_outcome(
        paste::decide(&content, &|p| p.exists()),
        retention,
        on_change,
    )
}

/// Queue whatever a decision said to ingest. Shared by the paste path and the
/// window's file drop -- the sequence decision -> queue -> kick is identical,
/// and two copies of it would eventually disagree about kicking.
pub fn ingest_outcome(
    outcome: paste::Outcome,
    retention: &retention::Retention,
    on_change: std::sync::Arc<dyn Fn(&str) + Send + Sync>,
) -> Result<PasteResult, String> {
    let paths = match outcome {
        paste::Outcome::Nothing(message) => {
            return Ok(PasteResult {
                queued: Vec::new(),
                message,
            })
        }
        paste::Outcome::Ingest(paths) => paths,
    };

    let queue = queue::Queue::open().map_err(|e| e.to_string())?;
    let mut handlers = handlers::QueueHandlers::new(queue, retention.clone());

    let mut queued = Vec::new();
    let mut failures = Vec::new();
    for path in &paths {
        // `origin: referenced` -- the user pointed at a file that already
        // existed. Framekeep never deletes those. The `copied` case belongs to
        // whoever writes bytes out, and nothing does yet.
        match handlers.ingest(&serde_json::json!({ "path": path.to_string_lossy() })) {
            Ok(reply) => {
                if let Some(handle) = reply["handle"].as_str() {
                    queued.push(handle.to_string());
                    // Kick the stage machine. Re-pasting mid-processing is a
                    // refresh, not a second job -- the kick's guard holds that.
                    let notify = on_change.clone();
                    pipeline::kick(handle.to_string(), move |h| notify(h));
                }
            }
            Err((_, message)) => failures.push(message),
        }
    }

    let message = if queued.is_empty() {
        failures
            .first()
            .cloned()
            .unwrap_or_else(|| "Nothing to paste. Copy a recording, then try again.".to_string())
    } else if queued.len() == 1 {
        "Added 1 recording to the queue.".to_string()
    } else {
        format!("Added {} recordings to the queue.", queued.len())
    };

    Ok(PasteResult { queued, message })
}

pub struct PasteResult {
    /// Handles now in the queue.
    pub queued: Vec<String>,
    /// What to tell the person. Always something.
    pub message: String,
}

/// Accept connections until [`Listener::shutdown`] is called.
///
/// One thread per connection, blocking reads. Two clients open at once --
/// Cursor and Claude Code, the case the plan names -- is the normal case, not
/// an edge one, so nothing here may assume a single session.
///
/// `new_handlers` is called once per connection. Each session gets its own,
/// because a session carries who it is talking to and that must never be
/// shared between connections.
pub fn serve<F>(listener: &Listener, new_handlers: F) -> std::io::Result<()>
where
    F: Fn() -> Box<dyn Handlers + Send> + Send + Sync + 'static,
{
    let factory = std::sync::Arc::new(new_handlers);
    loop {
        match listener.accept()? {
            None => return Ok(()),
            Some(conn) => {
                let factory = factory.clone();
                std::thread::spawn(move || {
                    let mut session = Session::new(factory());
                    // A connection dying is routine: a client quit, a laptop
                    // slept. It ends this thread and nothing else.
                    let _ = session.serve(conn);
                });
            }
        }
    }
}
