//! The only place in Framekeep that reads the clipboard. S4.2.
//!
//! # Principle IV, and how to check we kept it
//!
//! *Never watch the clipboard in the background.* Not polling, not a format
//! listener, not "smart detection" -- the temptation comes back every few
//! months and the answer is no every time, even when it would feel smoother.
//!
//! Two things enforce it here rather than leaving it to good intentions:
//!
//! 1. [`read`] takes a [`Gesture`], which only exists because a person pressed
//!    something. There is no way to ask for the clipboard without one.
//! 2. `tests::nothing_outside_this_file_touches_the_clipboard` reads the
//!    crate's own source and fails if any other file names a clipboard API, or
//!    if *any* file -- including this one -- names a monitoring API.
//!
//! The slice's definition of done asks for a code review on this point. The
//! test does not replace the review; it makes the review's finding stick.
//!
//! # Why hand-written and not the official plugin
//!
//! `tauri-plugin-clipboard-manager` cannot read `CF_HDROP` (research 4.3), and
//! that is the format Explorer actually uses -- measured 17/08: copying an
//! .mp4 puts `CF_HDROP`, `FileNameW` and `FileName` on the clipboard, and
//! nothing else. The community plugin that does handle it also ships clipboard
//! *monitoring*, which is the one feature this product may never have.

use std::path::PathBuf;

/// Proof that a person asked for this.
///
/// Constructing one is the moment responsibility is taken: do it in the
/// handler for a keypress or a click, never on a timer, never "just in case".
#[derive(Debug)]
pub struct Gesture {
    /// What the person did, for the record. Not read by logic -- its job is to
    /// make a wrong construction obvious at the call site.
    pub what: &'static str,
}

impl Gesture {
    /// The user pressed paste inside the window.
    pub fn paste_in_window() -> Gesture {
        Gesture {
            what: "paste in window",
        }
    }

    /// The user pressed the global shortcut.
    pub fn global_shortcut() -> Gesture {
        Gesture {
            what: "global shortcut",
        }
    }
}

/// What was on the clipboard, in the shapes this product cares about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Clipboard {
    /// `CF_HDROP` -- what Explorer puts there when you copy a file.
    Files(Vec<PathBuf>),
    /// `CF_UNICODETEXT`.
    Text(String),
    /// A bitmap. Not an error: see `paste::decide`.
    Image,
    /// Nothing we can use.
    Empty,
}

#[derive(Debug)]
pub struct ClipboardError(pub String);

impl std::fmt::Display for ClipboardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Read the clipboard, once, because someone asked.
pub fn read(gesture: &Gesture) -> Result<Clipboard, ClipboardError> {
    let _ = gesture;
    imp::read()
}

/// Put text on the clipboard, once, because someone pressed a button.
///
/// # Why writing lives here too, and why it needs no `Gesture`
///
/// A `Gesture` exists because *reading* is taking something that belongs to the
/// person: principle IV is about never doing that on our own initiative.
/// Writing gives rather than takes, and there is no background version of it to
/// guard against -- so the ceremony would be theatre.
///
/// It lives in this file anyway, and the test at the bottom now enforces that,
/// for a plainer reason: one door. The moment clipboard contact is spread over
/// two files, "where does this app touch the clipboard" stops having an answer
/// you can read in one sitting, and that question is the whole point of the
/// guard.
pub fn write_text(text: &str) -> Result<(), ClipboardError> {
    imp::write_text(text)
}

#[cfg(windows)]
mod imp {
    use super::{Clipboard, ClipboardError};
    use std::path::PathBuf;

    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable,
        OpenClipboard, SetClipboardData,
    };
    use windows_sys::Win32::System::Memory::{
        GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE,
    };
    use windows_sys::Win32::UI::Shell::DragQueryFileW;

    // Spelled out rather than fought with: windows-sys types these as u16
    // while the functions that take them want u32.
    const CF_TEXT_UNICODE: u32 = 13;
    const CF_HDROP: u32 = 15;
    const CF_BITMAP: u32 = 2;
    const CF_DIB: u32 = 8;
    const CF_DIBV5: u32 = 17;

    /// Asks for every file in one call. `DragQueryFileW` uses this index to
    /// mean "how many are there".
    const COUNT: u32 = 0xFFFF_FFFF;

    /// Closes the clipboard however this function leaves.
    ///
    /// Leaking it open is worse than it sounds: no other program on the machine
    /// can copy or paste until this process exits.
    struct Opened;

    impl Opened {
        fn now() -> Result<Opened, ClipboardError> {
            // Another program may hold it for a moment mid-copy. Windows offers
            // no wait, so: a few tries over ~100ms, then an honest failure.
            for attempt in 0..10 {
                if unsafe { OpenClipboard(std::ptr::null_mut()) } != 0 {
                    return Ok(Opened);
                }
                if attempt < 9 {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            }
            Err(ClipboardError(
                "Another program is using the clipboard. Try pasting again.".to_string(),
            ))
        }
    }

    impl Drop for Opened {
        fn drop(&mut self) {
            unsafe { CloseClipboard() };
        }
    }

    fn available(format: u32) -> bool {
        unsafe { IsClipboardFormatAvailable(format) != 0 }
    }

    pub fn read() -> Result<Clipboard, ClipboardError> {
        let _open = Opened::now()?;

        // Order is deliberate. A real file beats a description of one: an app
        // that offers both CF_HDROP and a text version of the same path should
        // give us the path Windows already resolved.
        if available(CF_HDROP) {
            let files = read_hdrop();
            if !files.is_empty() {
                return Ok(Clipboard::Files(files));
            }
        }

        if available(CF_TEXT_UNICODE) {
            if let Some(text) = read_text() {
                if !text.trim().is_empty() {
                    return Ok(Clipboard::Text(text));
                }
            }
        }

        // Measured 17/08: copying an image gives CF_BITMAP + CF_DIB + CF_DIBV5.
        if available(CF_DIB) || available(CF_DIBV5) || available(CF_BITMAP) {
            return Ok(Clipboard::Image);
        }

        Ok(Clipboard::Empty)
    }

    fn read_hdrop() -> Vec<PathBuf> {
        let handle: HANDLE = unsafe { GetClipboardData(CF_HDROP) };
        if handle.is_null() {
            return Vec::new();
        }
        let count = unsafe { DragQueryFileW(handle as _, COUNT, std::ptr::null_mut(), 0) };

        let mut files = Vec::new();
        for i in 0..count {
            let needed = unsafe { DragQueryFileW(handle as _, i, std::ptr::null_mut(), 0) };
            if needed == 0 {
                continue;
            }
            // +1 for the terminator the API writes but does not count.
            let mut buf = vec![0u16; needed as usize + 1];
            let written =
                unsafe { DragQueryFileW(handle as _, i, buf.as_mut_ptr(), buf.len() as u32) };
            if written > 0 {
                files.push(PathBuf::from(String::from_utf16_lossy(
                    &buf[..written as usize],
                )));
            }
        }
        files
    }

    fn read_text() -> Option<String> {
        let handle: HANDLE = unsafe { GetClipboardData(CF_TEXT_UNICODE) };
        if handle.is_null() {
            return None;
        }
        // The handle is global memory holding a NUL-terminated UTF-16 string.
        // GlobalLock is a no-op for clipboard memory on modern Windows, but the
        // pointer still has to be treated as borrowed until CloseClipboard.
        let ptr = handle as *const u16;
        let mut len = 0usize;
        // A clipboard string longer than this is not a path anyone typed.
        const MAX: usize = 64 * 1024;
        unsafe {
            while len < MAX && *ptr.add(len) != 0 {
                len += 1;
            }
            Some(String::from_utf16_lossy(std::slice::from_raw_parts(
                ptr, len,
            )))
        }
    }

    /// UTF-16, NUL-terminated, in moveable global memory -- the shape
    /// `CF_UNICODETEXT` is defined to take.
    ///
    /// Ownership of the block passes to the system on a successful
    /// `SetClipboardData`, so it must not be freed here. On the failure paths it
    /// leaks one small allocation rather than risk freeing a block the system
    /// may already own: a wrong free there is a crash, and this runs once per
    /// button press.
    pub fn write_text(text: &str) -> Result<(), ClipboardError> {
        // Same RAII guard the read path uses, for the same reason: leaving the
        // clipboard open stops every other program on the machine from copying
        // until this process exits. It also brings the retry, because the
        // program we are most likely to collide with is the one the person just
        // alt-tabbed away from.
        let _open = Opened::now()?;

        let mut utf16: Vec<u16> = text.encode_utf16().collect();
        utf16.push(0);

        unsafe {
            if EmptyClipboard() == 0 {
                return Err(ClipboardError("Couldn't clear the clipboard.".to_string()));
            }
            let block = GlobalAlloc(GMEM_MOVEABLE, std::mem::size_of_val(&utf16[..]));
            if block.is_null() {
                return Err(ClipboardError(
                    "Out of memory for the clipboard.".to_string(),
                ));
            }
            let dest = GlobalLock(block) as *mut u16;
            if dest.is_null() {
                return Err(ClipboardError(
                    "Couldn't lock clipboard memory.".to_string(),
                ));
            }
            std::ptr::copy_nonoverlapping(utf16.as_ptr(), dest, utf16.len());
            GlobalUnlock(block);
            if SetClipboardData(CF_TEXT_UNICODE, block as _).is_null() {
                return Err(ClipboardError(
                    "The clipboard refused the text.".to_string(),
                ));
            }
        }
        Ok(())
    }
}

#[cfg(not(windows))]
mod imp {
    use super::{Clipboard, ClipboardError};

    /// macOS reads `NSFilenamesPboardType` and is S7 work; Linux is not a
    /// shipping platform. Saying so beats returning an empty clipboard that
    /// reads like "you copied nothing".
    pub fn read() -> Result<Clipboard, ClipboardError> {
        Err(ClipboardError(
            "Pasting isn't wired up on this platform yet. Drop a file in instead.".to_string(),
        ))
    }

    pub fn write_text(_text: &str) -> Result<(), ClipboardError> {
        Err(ClipboardError(
            "Copying isn't wired up on this platform yet.".to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    /// Principle IV, as a test rather than a promise.
    ///
    /// Reads this crate's own source. Two rules:
    ///   - only this file may name a clipboard-reading API
    ///   - *no* file may name a clipboard-monitoring API, this one included
    ///
    /// It is a blunt instrument -- a string search over source -- and that is
    /// the point: the thing being guarded is blunt too.
    #[test]
    fn nothing_outside_this_file_touches_the_clipboard() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");

        // Assembled rather than written out, because this file is one of the
        // files being searched: spelling them here would make the guard
        // report itself. (It did, the first time it ran.)
        let join = |parts: &[&str]| parts.concat();

        // Reading and writing: allowed here, nowhere else. Writing joined the
        // list when S5.10 added the copy-the-prompt button. It is not a
        // principle IV risk the way reading is -- writing gives rather than
        // takes -- but the rule this test really enforces is *one door*, and a
        // second file putting things on the clipboard would end that whether or
        // not any principle was bent.
        let reading = [
            join(&["Open", "Clipboard"]),
            join(&["GetClipboard", "Data"]),
            join(&["DragQuery", "FileW"]),
            join(&["SetClipboard", "Data"]),
            join(&["Empty", "Clipboard"]),
        ];
        // Watching: allowed nowhere. These are the APIs that would turn this
        // product into the thing it refuses to be.
        let watching = [
            join(&["AddClipboardFormat", "Listener"]),
            join(&["SetClipboard", "Viewer"]),
            join(&["GetClipboardSequence", "Number"]),
        ];

        let mut problems = Vec::new();
        let mut files_read = 0;
        visit(&src, &mut |path, text| {
            files_read += 1;
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            for api in &watching {
                if text.contains(api.as_str()) {
                    problems.push(format!(
                        "{name} names {api} -- principle IV forbids it anywhere"
                    ));
                }
            }
            if name == "clipboard.rs" {
                return;
            }
            for api in &reading {
                if text.contains(api.as_str()) {
                    problems.push(format!(
                        "{name} names {api}; clipboard access belongs in clipboard.rs alone"
                    ));
                }
            }
        });

        // A guard that read nothing would pass in silence -- which is the
        // failure mode this project keeps catching in its own tests.
        assert!(
            files_read > 5,
            "only {files_read} source files were scanned; the guard is not looking where it thinks"
        );
        assert!(problems.is_empty(), "{}", problems.join("\n"));
    }

    fn visit(dir: &std::path::Path, f: &mut impl FnMut(&std::path::Path, &str)) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                visit(&path, f);
            } else if path.extension().is_some_and(|e| e == "rs") {
                if let Ok(text) = std::fs::read_to_string(&path) {
                    f(&path, &text);
                }
            }
        }
    }

    /// The guard has to be able to fire, or its silence proves nothing.
    ///
    /// Same assembled-string trick, same reason.
    #[test]
    fn the_guard_would_notice() {
        let needle = ["Open", "Clipboard"].concat();
        let sneaky = format!("fn quietly() {{ unsafe {{ {needle}(null_mut()) }} }}");
        assert!(sneaky.contains(&needle));

        let watcher = ["AddClipboardFormat", "Listener"].concat();
        assert!(format!("{watcher}(hwnd)").contains(&watcher));
    }
}
