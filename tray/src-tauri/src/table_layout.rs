//! One rule about the queue table, held by a test instead of by arithmetic.
//!
//! **The scroll floor must be at least what the columns actually need.**
//!
//! The table is a grid of fixed tracks inside a horizontally scrolling box.
//! Two numbers describe the same thing from different sides: the track list
//! says how wide a row is, and `min-width` on `.thead, #rows` says how far the
//! box may be scrolled. When `min-width` is the smaller of the two, the
//! difference is not scrolled to -- it is **clipped**, and clipped off the
//! right-hand end, where the row keeps its two buttons.
//!
//! That is what happened. On 18/08 the copy-prompt button moved out of the `⋯`
//! menu onto the row, which added a 32px track and a 10px gap; `min-width`
//! stayed at 950 while the tracks grew to 960. For a day the last 10px of `⋯`
//! could not be reached at any window size, and at the default window both
//! action buttons sat off the right edge behind a scrollbar. The owner
//! reported them as missing from the row, which is precisely what they were.
//!
//! Nothing about that was visible in the CSS: both numbers looked deliberate,
//! and neither one is wrong on its own. Only their relationship was, so the
//! relationship is what this checks.

#[cfg(test)]
mod tests {
    fn style_css() -> String {
        std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("ui")
                .join("style.css"),
        )
        .expect("ui/style.css")
    }

    /// The value of one declaration inside the rule that begins at `from`.
    fn declaration(css: &str, from: usize, name: &str) -> Option<String> {
        let block_end = css[from..].find('}')? + from;
        let at = css[from..block_end].find(&format!("{name}:"))? + from;
        let end = css[at..block_end].find(';')? + at;
        Some(css[at + name.len() + 1..end].trim().to_string())
    }

    /// Pixel widths from a `grid-template-columns` list. `minmax(180px, 1.6fr)`
    /// contributes its **minimum**, 180 -- that is the width the track falls to
    /// when the box is at its narrowest, which is the only case this test is
    /// about.
    fn track_minimums(list: &str) -> Vec<f64> {
        let mut out = Vec::new();
        let mut rest = list;
        while let Some(i) = rest.find("px") {
            // Walk back over the number in front of `px`.
            let head = &rest[..i];
            let start = head
                .rfind(|c: char| !c.is_ascii_digit() && c != '.')
                .map(|p| p + 1)
                .unwrap_or(0);
            if let Ok(n) = head[start..].parse::<f64>() {
                out.push(n);
            }
            rest = &rest[i + 2..];
            // Inside a minmax the first px value is the minimum and the second
            // half is an `fr`; there is no second px to skip, so nothing else
            // is needed here. A future `minmax(180px, 400px)` would count both,
            // which would make this test stricter rather than wrong.
        }
        out
    }

    #[test]
    fn the_table_can_be_scrolled_to_its_own_last_column() {
        let css = style_css();

        let grid_at = css
            .find(".thead, .row {")
            .expect("the row grid rule moved; this test names it directly");
        let tracks = declaration(&css, grid_at, "grid-template-columns")
            .expect("grid-template-columns in .thead, .row");
        let gap = declaration(&css, grid_at, "gap").expect("gap in .thead, .row");
        let padding = declaration(&css, grid_at, "padding").expect("padding in .thead, .row");

        let floor_at = css
            .find(".thead, #rows {")
            .expect("the min-width rule moved; this test names it directly");
        let min_width =
            declaration(&css, floor_at, "min-width").expect("min-width in .thead, #rows");

        let widths = track_minimums(&tracks);
        assert!(
            widths.len() >= 6,
            "read {} tracks out of {tracks:?} -- the parser is broken, not the CSS",
            widths.len()
        );

        let gap_px = track_minimums(&gap).first().copied().unwrap_or(0.0);
        // `padding: 0 16px` -- one value, applied to both sides.
        let side_padding = track_minimums(&padding).first().copied().unwrap_or(0.0) * 2.0;
        let floor = track_minimums(&min_width).first().copied().unwrap_or(0.0);

        let needed =
            widths.iter().sum::<f64>() + gap_px * (widths.len() as f64 - 1.0) + side_padding;

        assert!(
            floor >= needed,
            "the row needs {needed}px at its narrowest ({} tracks + {} gaps of {gap_px} + {side_padding} padding) \
             and .thead, #rows can only be scrolled to {floor}px. The last {}px of the row -- \
             which is where its buttons are -- cannot be reached at any window size.",
            widths.len(),
            widths.len() - 1,
            needed - floor
        );
    }

    /// The other half of the same story: fitting is not just about not
    /// clipping. The row's last two controls are the step after approving, and
    /// a step that is only reachable by scrolling sideways is one most people
    /// will never take.
    ///
    /// 932px is the table box in the default 1280px window: 1280 less the
    /// 290px sidebar, its border, and the main area's 28px padding on each
    /// side. Measured in the window, not derived from the mockup.
    #[test]
    fn the_whole_row_fits_the_default_window() {
        const TABLE_BOX_AT_DEFAULT_WINDOW: f64 = 932.0;
        let css = style_css();
        let floor_at = css.find(".thead, #rows {").expect(".thead, #rows rule");
        let min_width =
            declaration(&css, floor_at, "min-width").expect("min-width in .thead, #rows");
        let floor = track_minimums(&min_width).first().copied().unwrap_or(0.0);

        assert!(
            floor <= TABLE_BOX_AT_DEFAULT_WINDOW,
            "the row needs {floor}px and the table box is {TABLE_BOX_AT_DEFAULT_WINDOW}px in the \
             default window, so every row ends behind a horizontal scrollbar. Narrower windows \
             are allowed to scroll; the default one is not."
        );
    }
}
