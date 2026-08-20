//! Rules about the window that are held by tests instead of by attention.
//!
//! **Every screen `showReceipt` can speak on must have somewhere to speak.**
//! **Every key the window answers must be named on the Shortcuts screen.**
//!
//! Both rules are the same rule underneath: the window has to be able to
//! account for itself. One is about what it says after you act, the other
//! about what it lets you do in the first place, and each was written after
//! finding the app quietly failing at it.
//!
//! This is here because it was broken, and broken in the way that costs most:
//! silently. `showReceipt` wrote to `#receipt`, which lives in the queue
//! footer. The review screen calls it too -- for "Saved", and for every error
//! either of its buttons can raise -- and while review is up the queue is
//! `hidden`, so that element renders at 0x0.
//!
//! So `Save & keep reviewing` did its work and said nothing, and a failed
//! `Send to chat` was indistinguishable from a button that had never been
//! wired. The app was reporting the whole time, into a box nobody could see.
//! The owner found it by using the app and reporting three dead buttons.
//!
//! # The first version of this test was the wrong test
//!
//! It carried a hand-written list of "actionable screens" -- and the list was
//! exactly the two screens already fixed. It could only ever confirm the work
//! just done. `setup.js` was misrouting an error the same way at that moment,
//! and the test had nothing to say about it, because I had not thought to type
//! that screen's name into my own list.
//!
//! So the list is gone. The requirement is now read out of `queue.js`: whatever
//! screen names appear at a `showReceipt` call site are the screens that must
//! be able to answer. Adding a call site with a new screen name is what makes
//! this test demand a new receipt line -- which is the actual rule, rather than
//! a snapshot of one afternoon's understanding of it.

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    fn ui_file(name: &str) -> String {
        std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("ui")
                .join(name),
        )
        .unwrap_or_else(|e| panic!("ui/{name}: {e}"))
    }

    /// Every screen id a `showReceipt` call can target: the default in its
    /// signature, plus every literal passed at a call site.
    ///
    /// Scoped to the argument list, not the whole file. The first draft matched
    /// any `"screen-…"` anywhere and duly reported two screens that merely get
    /// shown and hidden by name -- both of which already speak through channels
    /// of their own (`#su-receipt`, and Settings' `say`). A guard that cries
    /// about things that are fine gets switched off, so it has to be exact.
    fn screens_spoken_on() -> BTreeSet<String> {
        let mut found = BTreeSet::new();
        for name in ["queue.js", "review.js", "setup.js", "settings.js"] {
            let js = ui_file(name);
            for (start, _) in js.match_indices("showReceipt(") {
                // Balanced scan: `showReceipt(String(e), "screen-review")` has
                // a nested call, and stopping at the first ')' would cut the
                // argument this whole test is about.
                let args_from = start + "showReceipt(".len();
                let mut depth = 1usize;
                let mut end = args_from;
                for (i, c) in js[args_from..].char_indices() {
                    match c {
                        '(' => depth += 1,
                        ')' => {
                            depth -= 1;
                            if depth == 0 {
                                end = args_from + i;
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                let args = &js[args_from..end];
                for (i, _) in args.match_indices("\"screen-") {
                    let tail = &args[i + 1..];
                    if let Some(q) = tail.find('"') {
                        found.insert(tail[..q].to_string());
                    }
                }
            }
        }
        found
    }

    #[test]
    fn every_screen_the_receipt_speaks_on_has_a_line_to_speak_from() {
        let html = ui_file("index.html");
        let screens = screens_spoken_on();

        // Before concluding anything: the scanner has to have found something.
        // An empty set would make the loop below vacuously pass, which is the
        // exact shape of test this repo has been bitten by more than once.
        assert!(
            screens.len() >= 2,
            "found {} screen ids at showReceipt call sites -- the scanner is broken, \
             not the UI. Found: {screens:?}",
            screens.len()
        );

        let missing: Vec<&String> = screens
            .iter()
            .filter(|id| {
                let Some(at) = html.find(&format!("id=\"{id}\"")) else {
                    return true;
                };
                let rest = &html[at..];
                let end = rest[1..]
                    .find("id=\"screen-")
                    .map(|i| i + 1)
                    .unwrap_or(rest.len());
                !rest[..end].contains("data-receipt")
            })
            .collect();

        assert!(
            missing.is_empty(),
            "showReceipt can be told to speak on these screens, and they have nowhere \
             to say it: {missing:?}. The message goes to a hidden element, the action \
             looks like it did nothing, and its errors vanish with it."
        );
    }

    /// Every key the window answers at the document level, lowercased.
    ///
    /// Document level is the line between an app shortcut and a control's own
    /// convention: `Enter` in the "always hide these words" box is part of that
    /// box, listed on the Shortcuts screen it would be noise. So the scan is
    /// scoped to `document.addEventListener("keydown", …)` bodies, which is
    /// also exactly where a new app-wide shortcut would be added.
    fn keys_answered_by_the_window() -> BTreeSet<String> {
        let mut found = BTreeSet::new();
        for name in ["queue.js", "review.js", "setup.js", "settings.js"] {
            let js = ui_file(name);
            for (start, _) in js.match_indices("document.addEventListener(\"keydown\"") {
                // To the end of the handler: the next top-level statement is
                // near enough, and handlers here are one screenful each.
                let body = &js[start..];
                let end = body
                    .find("\n});")
                    .map(|i| i + 3)
                    .unwrap_or(body.len().min(3000));
                for key in key_literals(&body[..end]) {
                    found.insert(key);
                }
            }
        }
        found
    }

    /// `.key === "Escape"` and `.key.toLowerCase() === "v"`, both to `escape`
    /// and `v`. Only comparisons against `.key`, so a string that merely looks
    /// like a key name cannot wander in.
    fn key_literals(src: &str) -> Vec<String> {
        let mut out = Vec::new();
        for (i, _) in src.match_indices(".key") {
            let rest = &src[i..];
            // Between `.key` and the literal there is at most `.toLowerCase()`
            // and ` === `. Anything longer is not a comparison.
            let Some(q) = rest.find('"') else { continue };
            if q > 24 {
                continue;
            }
            let tail = &rest[q + 1..];
            if let Some(close) = tail.find('"') {
                out.push(tail[..close].to_lowercase());
            }
        }
        out
    }

    /// What the Shortcuts section says it answers: the base key of each row's
    /// `data-keys`, modifiers stripped, one row allowed to list several.
    fn keys_the_screen_names() -> BTreeSet<String> {
        let html = ui_file("index.html");
        let mut found = BTreeSet::new();
        for (i, _) in html.match_indices("data-keys=\"") {
            let tail = &html[i + "data-keys=\"".len()..];
            let Some(close) = tail.find('"') else {
                continue;
            };
            for chord in tail[..close].split(',') {
                if let Some(base) = chord.trim().rsplit('+').next() {
                    found.insert(base.to_string());
                }
            }
        }
        found
    }

    /// The Shortcuts section is the app's only account of its own keyboard, and
    /// an account that drifts is worse than none: it teaches a chord that does
    /// nothing, or stays silent about one that works.
    ///
    /// This is the shape of S5.9's whole reason for existing. `Ctrl+Shift+V`
    /// worked from the day the tray shipped and was written down in exactly one
    /// place -- the empty-queue hint, which disappears for good at the first
    /// recording. A feature nobody can find is not shipped.
    #[test]
    fn the_shortcuts_screen_names_every_key_the_window_answers() {
        let answered = keys_answered_by_the_window();
        let named = keys_the_screen_names();

        // Neither scanner may be silently empty -- a vacuous pass here would
        // let the screen and the handlers drift apart in perfect agreement.
        assert!(
            answered.len() >= 3,
            "found {answered:?} at document keydown handlers -- the scanner is broken, not the UI"
        );
        assert!(
            named.len() >= 3,
            "found {named:?} in the Shortcuts section -- the scanner is broken, not the UI"
        );

        let undocumented: Vec<&String> = answered.difference(&named).collect();
        assert!(
            undocumented.is_empty(),
            "the window answers {undocumented:?} and the Shortcuts section never mentions \
             them. A shortcut nobody is told about is a feature that was not shipped."
        );

        let promised: Vec<&String> = named.difference(&answered).collect();
        assert!(
            promised.is_empty(),
            "the Shortcuts section names {promised:?} and no document-level handler answers \
             them. Teaching a chord that does nothing is worse than teaching none."
        );
    }

    /// The one shortcut that is not the window's to answer: it is registered
    /// with the OS in `gui.rs`, works while the window is hidden, and is the
    /// only reason the app is usable from the tray at all.
    #[test]
    fn the_global_chord_on_the_screen_is_the_chord_that_gets_registered() {
        let html = ui_file("index.html");
        let gui = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("src")
                .join("gui.rs"),
        )
        .expect("gui.rs");

        assert!(
            gui.contains("Modifiers::CONTROL | Modifiers::SHIFT") && gui.contains("Code::KeyV"),
            "gui.rs no longer registers Ctrl+Shift+V; the Shortcuts screen still promises it"
        );
        assert!(
            html.contains("data-keys=\"ctrl+shift+v\""),
            "gui.rs registers Ctrl+Shift+V and the Shortcuts screen does not list it"
        );
    }

    /// The marker is the contract between the markup and the script. If either
    /// side renames it, every receipt goes quiet again without a compile error.
    #[test]
    fn the_receipt_marker_matches_what_the_script_looks_for() {
        let js = ui_file("queue.js");
        let html = ui_file("index.html");

        assert!(
            js.contains("[data-receipt]"),
            "queue.js no longer queries [data-receipt]; the markers in index.html are dead weight"
        );
        assert!(
            html.matches("data-receipt").count() >= 2,
            "expected a receipt line per screen showReceipt speaks on, found {}",
            html.matches("data-receipt").count()
        );
    }
}
