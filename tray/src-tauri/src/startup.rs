//! "Start Framekeep on system startup", which Windows owns and we only ask.
//!
//! A packaged app declares a `startupTask` in its manifest; the system then
//! decides whether it actually runs, and a person can veto it in Task Manager
//! without telling the app. So this module asks and reports, and never assumes
//! its own settings file is the truth.
//!
//! # Why not a Run key
//!
//! The unpackaged shortcut is a value under
//! `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`, and it is deliberately
//! not used. It survives uninstalling the app, it is invisible to the Startup
//! tab a user would go looking in, and a Store app writing one is the kind of
//! thing certification asks about. Outside a package the honest answer is that
//! there is no startup task to toggle -- which is what `available` reports, so
//! the window can say so instead of pretending.

#[cfg(windows)]
mod imp {
    use windows::core::{RuntimeType, HSTRING};
    use windows::ApplicationModel::{StartupTask, StartupTaskState};
    use windows_future::{AsyncOperationCompletedHandler, IAsyncOperation};

    /// The id in the manifest's `uap5:StartupTask`. Changing one without the
    /// other silently turns the switch into a no-op.
    const TASK_ID: &str = "FramekeepStartup";

    /// Wait for a WinRT async operation, through the surface that is public.
    ///
    /// The same trap `core::ocr` hit and wrote down: `windows-future`'s readme
    /// documents `Async::join`, and that trait is imported privately in its own
    /// `lib.rs`, so the documented call does not compile. Subscribing after
    /// starting is not a race -- WinRT calls the handler immediately when the
    /// operation has already finished.
    fn block_on<T: RuntimeType>(op: IAsyncOperation<T>) -> windows::core::Result<T> {
        let (done, wait) = std::sync::mpsc::channel();
        op.SetCompleted(&AsyncOperationCompletedHandler::new(move |_, _| {
            let _ = done.send(());
            Ok(())
        }))?;
        let _ = wait.recv();
        op.GetResults()
    }

    fn task() -> Option<StartupTask> {
        StartupTask::GetAsync(&HSTRING::from(TASK_ID))
            .and_then(block_on)
            .ok()
    }

    /// Whether this build can offer the switch at all. False when running
    /// unpackaged, where there is no startup task to ask about.
    pub fn available() -> bool {
        task().is_some()
    }

    /// What the system says today -- not what our settings file remembers.
    ///
    /// `DisabledByUser` and `DisabledByPolicy` both read as off, and both are
    /// answers we must not argue with: asking to enable a task the user
    /// disabled by hand is refused by Windows, correctly.
    pub fn enabled() -> bool {
        task()
            .and_then(|t| t.State().ok())
            .map(|s| s == StartupTaskState::Enabled)
            .unwrap_or(false)
    }

    /// Ask Windows to enable or disable it. The caller reads `enabled()` back,
    /// because asking is not the same as getting.
    pub fn set(on: bool) -> Result<(), String> {
        let Some(task) = task() else {
            return Err(
                "Starting with Windows needs the installed version of Framekeep.".to_string(),
            );
        };
        if on {
            let state = task
                .RequestEnableAsync()
                .and_then(block_on)
                .map_err(|e| format!("Windows refused to enable the startup task ({e})."))?;
            if state != StartupTaskState::Enabled {
                // The one case worth a sentence of its own: a user who turned
                // this off in Task Manager cannot be overridden from here, and
                // saying "done" would be a lie the next reboot exposes.
                return Err(
                    "Windows is blocking Framekeep from starting automatically. \
                     Turn it back on under Startup apps in Task Manager."
                        .to_string(),
                );
            }
        } else {
            task.Disable()
                .map_err(|e| format!("Windows refused to disable the startup task ({e})."))?;
        }
        Ok(())
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn available() -> bool {
        false
    }
    pub fn enabled() -> bool {
        false
    }
    pub fn set(_on: bool) -> Result<(), String> {
        Err("Starting on login is a Windows feature for now.".to_string())
    }
}

pub use imp::{available, enabled, set};
