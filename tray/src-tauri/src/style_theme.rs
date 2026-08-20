//! One rule about `ui/style.css`, held by the compiler instead of by memory.
//!
//! The dark palette is declared twice: once inside
//! `@media (prefers-color-scheme: dark)` so a dark OS gets it, and once under a
//! flat `:root[data-theme="dark"]` so the Settings toggle can force it on a
//! light OS. The media query alone cannot do the second job -- on a light OS it
//! never matches, so nothing inside it exists to be selected, and the app stays
//! light while Settings says Dark.
//!
//! That was a real bug, and it survived because of how it hid: the machine the
//! screens were built and photographed on was itself in dark mode, so the
//! broken direction was the one nobody was in a position to see. The screenshots
//! were evidence that the theme worked, taken in the only configuration where
//! the question does not get asked.
//!
//! Duplication fixes it and introduces the obvious hazard: someone edits one
//! block. This module is the answer to that -- it reads the stylesheet and
//! fails if the two stop agreeing, which turns a silent half-theme into a red
//! test.

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    /// Every `--custom-property: value;` inside one brace-delimited block,
    /// starting from the byte offset of its opening `{`.
    fn declarations(css: &str, open_brace: usize) -> BTreeMap<String, String> {
        let mut depth = 0usize;
        let mut end = css.len();
        for (i, c) in css[open_brace..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = open_brace + i;
                        break;
                    }
                }
                _ => {}
            }
        }

        let mut out = BTreeMap::new();
        for line in css[open_brace..end].lines() {
            let line = line.trim();
            let Some(rest) = line.strip_prefix("--") else {
                continue;
            };
            let Some((name, value)) = rest.split_once(':') else {
                continue;
            };
            out.insert(
                name.trim().to_string(),
                value.trim().trim_end_matches(';').trim().to_string(),
            );
        }
        out
    }

    /// Offset of the `{` that opens the block introduced by `selector`.
    fn block_after(css: &str, selector: &str) -> usize {
        let at = css
            .find(selector)
            .unwrap_or_else(|| panic!("style.css no longer contains `{selector}`"));
        at + css[at..]
            .find('{')
            .unwrap_or_else(|| panic!("`{selector}` is not followed by a block"))
    }

    #[test]
    fn both_dark_blocks_declare_the_same_palette() {
        let css = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("ui")
                .join("style.css"),
        )
        .expect("ui/style.css");

        let media = declarations(
            &css,
            block_after(&css, ":root:not([data-theme=\"light\"]) {"),
        );
        let forced = declarations(&css, block_after(&css, ":root[data-theme=\"dark\"] {"));

        // The assertion that makes the two above worth reading: a parser that
        // silently matched nothing would report two empty maps as equal, which
        // is the shape of test this repo has been bitten by before.
        assert!(
            media.len() > 20,
            "parsed only {} properties from the media block -- the parser, not the CSS, is broken",
            media.len()
        );
        assert!(
            media.contains_key("bg") && media.contains_key("error"),
            "the parsed block does not look like the palette"
        );

        assert_eq!(
            media, forced,
            "the two dark blocks in ui/style.css have drifted apart. \
             A value in one and not the other means the Settings theme toggle \
             and a dark OS render different apps. Edit one, edit the other."
        );
    }

    /// The three rules that are not tokens but still only exist under the media
    /// query. Same failure, smaller blast radius, and easy to forget when
    /// adding a fourth.
    #[test]
    fn dark_only_rules_are_mirrored_for_the_forced_theme() {
        let css = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("ui")
                .join("style.css"),
        )
        .expect("ui/style.css");

        let mut missing = Vec::new();
        let mut checked = 0;
        for line in css.lines() {
            let line = line.trim();
            let Some(rest) = line.strip_prefix(":root:not([data-theme=\"light\"])") else {
                continue;
            };
            // The palette block itself is covered by the test above.
            if rest.trim() == "{" {
                continue;
            }
            checked += 1;
            let selector_tail = rest.split('{').next().unwrap_or("").trim();
            let twin = format!(":root[data-theme=\"dark\"] {selector_tail}");
            if !css.contains(&twin) {
                missing.push(twin);
            }
        }

        assert!(
            checked >= 2,
            "found {checked} dark-only rules to check -- expected the parser to find several"
        );
        assert!(
            missing.is_empty(),
            "these dark-only rules apply on a dark OS but not when the theme is \
             forced to Dark: {missing:?}"
        );
    }
}
