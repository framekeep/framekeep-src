//! UI strings the Rust side owns: the tray menu and tooltip.
//!
//! Source of truth is `_design_system/copy.md`, section "Tray" -- change there
//! first, then here. English only; no hardcoded strings anywhere else in this
//! crate's UI code. The window's strings live in `ui/index.html`, same rule.

pub const TRAY_TOOLTIP: &str = "Framekeep";
pub const TRAY_OPEN: &str = "Open Framekeep";
pub const TRAY_QUIT: &str = "Quit Framekeep";

/// The command that connects an AI client to Framekeep, run once per project.
///
/// In Rust rather than in the markup for one reason: the Setup screen both
/// **shows** this line and offers a button that **copies** it, and a copy
/// button that puts something other than what is on screen onto the clipboard
/// is a trap nobody would think to check for. One string, read twice.
///
/// Why a typed command at all, when the app is right there: the MCP adapter
/// ships from npm and is not inside the package yet, so the app has nothing to
/// point a client at. Typing it in a terminal is also the one place `npx`
/// works on Windows -- a shell exists there, and
/// `docs/experiments/npx-spawn-windows.md` measured what happens without one.
/// What `init` then writes never contains `npx`: it points at an absolute
/// path, which is what keeps the daily path clear of the whole problem.
pub const CONNECT_COMMAND: &str = "npx framekeep-mcp init";

/// The line put on the clipboard for the person to paste into their AI client.
///
/// Kept here with the tray strings rather than in the window, because the
/// window is not the only thing that will want it -- and because this sentence
/// is the whole of the product's fourth step, the one no screen had said out
/// loud until 18/08: approving unlocks a recording, and a model still has to
/// ask for it by path.
///
/// Deliberately a plain request naming the file. The tool descriptions already
/// tell a model to call `video_map` first, so instructing it here would be a
/// second copy of that rule, drifting from the first.
pub fn ai_prompt(path: &str) -> String {
    format!("Look at the screen recording at \"{path}\" and tell me what happens in it.")
}

#[cfg(test)]
mod prompt_tests {
    #[test]
    fn the_prompt_carries_the_path_and_quotes_it() {
        // Quoted because the paths this product deals with have spaces in them
        // by nature -- `C:\Users\Nguyen Van A\Videos\test.mp4` is the standing
        // test case in AGENTS.md, and an unquoted path breaks at the space in
        // whatever the client does with it.
        let p = r"C:\Users\Nguyen Van A\Videos\test.mp4";
        let line = super::ai_prompt(p);
        assert!(line.contains(p), "the path is the point: {line}");
        assert!(
            line.contains(&format!("\"{p}\"")),
            "path must be quoted: {line}"
        );
    }
}
