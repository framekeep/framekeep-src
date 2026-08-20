//! Finding secrets in what OCR read off a screen. S5.2.
//!
//! # Why these are not the gitleaks patterns
//!
//! The plan says to import the pattern sets from gitleaks and detect-secrets,
//! and that is still where the *shapes* come from. But those tools scan files,
//! where a byte is a byte. Applied unchanged to OCR output they found 44% of
//! planted secrets; reshaped around what the engine actually does, this scanner
//! finds **83% at 16px, 62% at 14px, 8% at 11px** -- and every remaining miss
//! at the readable sizes was checked by hand and is a line OCR never produced,
//! not a pattern that failed (`docs/experiments/ocr-windows-media.md`).
//!
//! What the measurement said the patterns have to survive:
//!
//! - **`_` comes back as a space, or not at all.** Whole runs vanish:
//!   `STRIPE_KEY=sk_live_51H8x…` was read as `STRIPE 51H8x…`, losing the
//!   entire prefix. So every separator is optional, and where a prefix can
//!   disappear the tail has to carry the detection.
//! - **Homoglyphs**: `B`↔`8`, `l`/`I`↔`1`, `O`↔`0`, `0`↔`e`, `S`↔`5`.
//! - **Spaces appear inside a key.** A key is scanned both as read and with
//!   spaces removed.
//!
//! # The other half: precision
//!
//! Loosening cost nothing measurable -- zero false positives across five real
//! screen recordings -- and that is a property worth keeping, because a tool
//! that blacks out ordinary words is broken in the other direction. Two rules
//! hold the line: card numbers must pass Luhn, and there is deliberately no
//! bare phone-number pattern (see `Kind`).
//!
//! # This module never decides what happens next
//!
//! It reports what it found and where. Masking is `redact.rs`, approval is a
//! person. At 80% recall nothing here may be described as complete, and the
//! product must not either.

use std::ops::Range;

/// The badge a person sees next to a finding. Names come from
/// `_design_system/copy.md`, so a new kind here needs a badge there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    ApiKey,
    Token,
    Email,
    CardNumber,
    PrivateKey,
    /// A word the person typed in Settings (S5.8). Nothing about its shape says
    /// "secret" -- what says so is that somebody asked for it.
    Custom,
    // Deliberately absent: phone numbers. Every loose phone pattern also
    // matches version strings, timestamps, order ids and IP addresses, and
    // zero false positives is a measured property of this scanner that a
    // phone regex would spend. `copy.md` lists a `Phone` badge; it stays
    // unused until there is a pattern that earns it.
}

impl Kind {
    /// English, and it is what the review screen shows.
    pub fn label(self) -> &'static str {
        match self {
            Kind::ApiKey => "API key",
            Kind::Token => "Token",
            Kind::Email => "Email address",
            Kind::CardNumber => "Card number",
            Kind::PrivateKey => "Private key",
            Kind::Custom => "Custom pattern",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    pub kind: Kind,
    /// Exactly what matched, as OCR read it -- not as it is on disk. Kept so
    /// the review screen can show which key this is without showing the key.
    pub text: String,
    /// Where it sits in the string that was scanned.
    pub span: Range<usize>,
}

impl Finding {
    /// `sk-••••••4f2a` -- enough to recognise which key, never enough to use.
    ///
    /// The mockup renders these in full; that is listed as a defect to fix
    /// (`mockup-notes.md`), and this is the fix.
    pub fn masked(&self) -> String {
        let chars: Vec<char> = self.text.chars().collect();
        if chars.len() <= 8 {
            return "•".repeat(chars.len().max(4));
        }
        let head: String = chars[..3].iter().collect();
        let tail: String = chars[chars.len() - 4..].iter().collect();
        format!("{head}{}{tail}", "•".repeat(6))
    }
}

/// One rule. Hand-written rather than regex: the crate has four dependencies
/// and none of them is a regex engine, and these shapes are prefix-plus-run,
/// which is a scan not a grammar.
struct Rule {
    kind: Kind,
    /// Accepted spellings of the prefix, already including the homoglyphs the
    /// engine was measured producing. Matched case-sensitively unless the
    /// real-world token is case-insensitive.
    prefixes: &'static [&'static str],
    /// How much alphanumeric run has to follow for this to be a secret rather
    /// than a coincidence.
    min_run: usize,
    case_insensitive: bool,
}

const RULES: &[Rule] = &[
    // OpenAI. `sk-proj-` and the older bare `sk-`.
    Rule {
        kind: Kind::ApiKey,
        prefixes: &["sk-proj-", "sk-pr0j-", "skproj"],
        min_run: 12,
        case_insensitive: false,
    },
    Rule {
        kind: Kind::ApiKey,
        prefixes: &["sk-"],
        min_run: 20,
        case_insensitive: false,
    },
    // Stripe. The prefix is the one measured vanishing entirely, so the rule
    // that catches it in practice is the long run after the word STRIPE --
    // handled by `scan_dense` finding `sk_live_` when it survives, and by the
    // generous `min_run` when it does not.
    Rule {
        kind: Kind::ApiKey,
        prefixes: &["sk_live_", "sk live ", "sklive", "pk_live_", "rk_live_"],
        min_run: 16,
        case_insensitive: false,
    },
    // GitHub. `_` measured coming back as a space or nothing.
    Rule {
        kind: Kind::Token,
        prefixes: &[
            "ghp_",
            "ghp ",
            "ghp",
            "gho_",
            "ghu_",
            "ghs_",
            "ghr_",
            "github_pat_",
        ],
        min_run: 20,
        case_insensitive: false,
    },
    // AWS access key id. `I` misread as `1` or `l` is the measured case.
    Rule {
        kind: Kind::ApiKey,
        prefixes: &["AKIA", "AK1A", "AKlA", "ASIA"],
        min_run: 12,
        case_insensitive: false,
    },
    // Slack.
    Rule {
        kind: Kind::Token,
        prefixes: &["xoxb-", "xoxp-", "xoxa-", "xoxr-", "xoxs-"],
        min_run: 10,
        case_insensitive: false,
    },
    // Google.
    Rule {
        kind: Kind::ApiKey,
        prefixes: &["AIza"],
        min_run: 30,
        case_insensitive: false,
    },
    // JWT. `B`->`8` measured on the neighbouring word, so allow it here too.
    Rule {
        kind: Kind::Token,
        prefixes: &["eyJ", "8yJ"],
        min_run: 20,
        case_insensitive: false,
    },
    // PEM headers. Whitespace inside is normal even without OCR.
    Rule {
        kind: Kind::PrivateKey,
        prefixes: &["-----BEGIN"],
        min_run: 0,
        case_insensitive: true,
    },
];

/// Characters that count as part of a secret's body.
fn is_body(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' || c == '/' || c == '+'
}

// --- custom patterns (S5.8) --------------------------------------------------

/// The shortest word that may be added. Three characters is `the`, and a
/// pattern that hides every word starting with `the` does not hide a secret,
/// it hides the screen -- and a review list of two hundred findings is one
/// nobody reads, which costs the 20% that only a person catches.
pub const MIN_PATTERN: usize = 4;

/// How many a person may keep. A cap because a settings file with two hundred
/// patterns in it has stopped being a preference and become a program, and
/// because every pattern is another pass over every frame.
pub const MAX_PATTERNS: usize = 20;

/// Collapse one character onto what OCR might have turned it into.
///
/// Every mapping here is a confusion the engine was *measured* making (module
/// header): `_` comes back as a space or vanishes, `B`↔`8`, `l`/`I`↔`1`,
/// `O`↔`0`, `0`↔`e`, `S`↔`5`. Folding both sides and comparing is what lets one
/// typed word match what OCR actually produced. The alternative -- generating
/// every spelling of the word -- is a cross-product: a twelve-character word
/// with ambiguous letters is thousands of strings to search for, per frame.
///
/// `o`, `0` and `e` land on the same character because the measurements chain
/// them together, so `code` and `c0d0` fold alike. That is deliberate and it
/// only ever widens what a person's own word catches; it cannot make an
/// unrelated word match one.
fn fold_char(c: char) -> Option<char> {
    match c.to_ascii_lowercase() {
        // Separators OCR eats, and the space it leaves behind.
        '_' | ' ' | '-' | '.' => None,
        'b' | '8' => Some('8'),
        'l' | 'i' | '1' | '|' => Some('1'),
        'o' | '0' | 'e' => Some('0'),
        's' | '5' => Some('5'),
        other => Some(other),
    }
}

fn fold(text: &str) -> Vec<char> {
    text.chars().filter_map(fold_char).collect()
}

/// Whether a word can be used as a custom pattern, and if not, the sentence a
/// person reads. English, because it goes on screen.
///
/// The length is checked *after* folding: `a_b_c` is five characters and three
/// letters, and it is the three that decide how much of a screen it covers.
pub fn check_pattern(word: &str) -> Result<String, String> {
    let trimmed = word.trim();
    if trimmed.is_empty() {
        return Err("Type a word or phrase to hide.".into());
    }
    if fold(trimmed).len() < MIN_PATTERN {
        return Err(format!(
            "Use at least {MIN_PATTERN} letters or digits. Shorter patterns match ordinary \
             words and bury the real findings."
        ));
    }
    Ok(trimmed.to_string())
}

/// Words a person added in Settings, plus whatever is attached to them.
///
/// One typed word does two jobs, because the person should not have to know
/// which one their secret needs:
///
/// - it is hidden **on its own**, so `Project Nightingale` works, and
/// - it drags along any run stuck to it, so `acme_` also covers
///   `acme_51H8xKqR…` -- the case a bare literal would miss by ten characters.
///
/// Two guards stop it blacking out a screen. The word must survive
/// `check_pattern`, and the match must begin at a boundary in the text *as
/// read*, so `acme` fires in `ACME KEY` and not inside `panacme`.
fn custom_words(text: &str, words: &[String], already: &[Finding]) -> Vec<Finding> {
    let mut out: Vec<Finding> = Vec::new();
    if words.is_empty() {
        return out;
    }

    // Fold the text once, remembering where each surviving character came
    // from, so a match in folded space maps back to real byte offsets.
    let mut folded: Vec<char> = Vec::new();
    let mut starts_at: Vec<usize> = Vec::new();
    let mut ends_at: Vec<usize> = Vec::new();
    for (at, c) in text.char_indices() {
        if let Some(f) = fold_char(c) {
            folded.push(f);
            starts_at.push(at);
            ends_at.push(at + c.len_utf8());
        }
    }

    for word in words {
        let needle = fold(word);
        if needle.len() < MIN_PATTERN || needle.len() > folded.len() {
            continue;
        }
        for i in 0..=(folded.len() - needle.len()) {
            if folded[i..i + needle.len()] != needle[..] {
                continue;
            }
            let start = starts_at[i];
            if text[..start].chars().next_back().is_some_and(is_body) {
                continue; // mid-word: not this pattern, just letters that rhyme
            }
            let mut end = ends_at[i + needle.len() - 1];
            end += text[end..]
                .chars()
                .take_while(|c| is_body(*c))
                .map(char::len_utf8)
                .sum::<usize>();

            let span = start..end;
            let taken = |f: &Finding| overlaps(&f.span, &span);
            if already.iter().any(taken) || out.iter().any(taken) {
                continue;
            }
            out.push(Finding {
                kind: Kind::Custom,
                text: text[start..end].to_string(),
                span,
            });
        }
    }
    out
}

/// The scanner with nobody's custom words -- the shape every measurement in
/// `docs/experiments/` was taken against, and what the tests below mean by
/// "scan". Production has one entry point, `scan_with`, so there is no second
/// path that could drift away from what was measured.
#[cfg(test)]
fn scan(text: &str) -> Vec<Finding> {
    scan_with(text, &[])
}

/// Scan a run of text, also honouring the words a person added in Settings (S5.8).
///
/// `custom` is passed in rather than read from anywhere: this crate is a CLI
/// that takes its whole world from its arguments, and a scanner that quietly
/// consulted the app's settings file would answer differently depending on who
/// was logged in.
pub fn scan_with(text: &str, custom: &[String]) -> Vec<Finding> {
    let mut found: Vec<Finding> = Vec::new();

    for rule in RULES {
        for prefix in rule.prefixes {
            let mut from = 0;
            while let Some(at) = find_at(text, prefix, from, rule.case_insensitive) {
                let start = at;
                let after = at + prefix.len();
                let run: String = text[after..].chars().take_while(|c| is_body(*c)).collect();
                from = after.max(at + 1);

                if run.len() < rule.min_run {
                    continue;
                }
                let end = after + run.len();
                let hit = Finding {
                    kind: rule.kind,
                    text: text[start..end].to_string(),
                    span: start..end,
                };
                // A longer rule already covering this span wins: `sk-proj-…`
                // must not also be reported as a bare `sk-…`.
                if !found.iter().any(|f| overlaps(&f.span, &hit.span)) {
                    found.push(hit);
                }
            }
        }
    }

    // Order is badge precedence: the built-in rules and the shapes that carry
    // their own meaning claim their spans first, so `quang@acme.vn` stays an
    // `Email address` for someone whose custom word is `acme`. The loosest rule
    // goes last for the same reason.
    found.extend(emails(text, &found));
    found.extend(card_numbers(text, &found));
    found.extend(custom_words(text, custom, &found));
    found.extend(labelled_runs(text, custom, &found));
    found.sort_by_key(|f| f.span.start);
    found
}

/// Words that mean "what follows is a credential".
///
/// Lowercased before comparison, and matched as a substring so `OPENAI_KEY`,
/// `apiKey` and `AUTH_TOKEN` all count.
/// Vendor names count too, and for the same reason: `STRIPE_SECRET_KEY=` and
/// `OPENAI_API_KEY=` are how these sit in a real `.env`, and OCR eating the
/// `_KEY=` part leaves the vendor name as the only surviving label.
const KEYWORDS: &[&str] = &[
    "key",
    "token",
    "secret",
    "password",
    "passwd",
    "credential",
    "auth",
    "bearer",
    "stripe",
    "github",
    "openai",
    "anthropic",
    "slack",
    "twilio",
    "sendgrid",
];

/// A long high-entropy run sitting next to a word like `KEY=`.
///
/// This is the only thing that can catch a secret whose prefix OCR destroyed.
/// The measurement had `STRIPE_KEY=sk_live_51H8x…` come back as
/// `STRIPE 51H8x…` -- the entire `sk_live_` gone. Nothing about the remaining
/// run says "Stripe"; what says it is the word `KEY` in front of it, which is
/// how detect-secrets' keyword detector works too.
///
/// Held to a narrow shape on purpose, because this is the rule most likely to
/// spend the scanner's zero-false-positive record:
///
/// - the run needs **both** cases **and** a digit, which excludes commit
///   hashes, hex, snake_case identifiers and ordinary words
/// - it has to be at least 20 characters
/// - a keyword has to appear within 24 characters in front of it
///
/// A person's own words (S5.8) join `KEYWORDS` here, which is the second job
/// one typed word does: `acme` catches `acme_51H8x…` as a prefix, and it also
/// catches `ACME KEY: 51H8xKqR…` where the run is not attached to anything.
/// Their words are compared folded, because they were typed by someone who has
/// never heard of what OCR does to an underscore.
fn labelled_runs(text: &str, extra: &[String], already: &[Finding]) -> Vec<Finding> {
    const MIN_RUN: usize = 20;
    const LOOK_BACK: usize = 24;

    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if !is_body(bytes[i] as char) {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && is_body(bytes[i] as char) {
            i += 1;
        }
        let run = &text[start..i];
        if run.len() < MIN_RUN {
            continue;
        }
        let has_upper = run.chars().any(|c| c.is_ascii_uppercase());
        let has_lower = run.chars().any(|c| c.is_ascii_lowercase());
        let has_digit = run.chars().any(|c| c.is_ascii_digit());
        if !(has_upper && has_lower && has_digit) {
            continue;
        }

        let from = start.saturating_sub(LOOK_BACK);
        let before = text[from..start].to_ascii_lowercase();
        let kind = if KEYWORDS.iter().any(|k| before.contains(k)) {
            Kind::ApiKey
        } else {
            let folded = fold(&before);
            let theirs = extra.iter().any(|theirs| {
                let word = fold(theirs);
                word.len() >= MIN_PATTERN
                    && word.len() <= folded.len()
                    && folded.windows(word.len()).any(|w| w == word.as_slice())
            });
            if !theirs {
                continue;
            }
            // Their word, their badge: calling it an API key would be this
            // module guessing at what somebody's own label means.
            Kind::Custom
        };

        let span = start..i;
        if already.iter().any(|f| overlaps(&f.span, &span)) {
            continue;
        }
        out.push(Finding {
            kind,
            text: run.to_string(),
            span,
        });
    }
    out
}

fn overlaps(a: &Range<usize>, b: &Range<usize>) -> bool {
    a.start < b.end && b.start < a.end
}

fn find_at(text: &str, needle: &str, from: usize, insensitive: bool) -> Option<usize> {
    if from >= text.len() {
        return None;
    }
    let hay = &text[from..];
    let at = if insensitive {
        hay.to_ascii_lowercase().find(&needle.to_ascii_lowercase())
    } else {
        hay.find(needle)
    }?;
    Some(from + at)
}

/// Email addresses, tolerating the spaces OCR sprinkles around dots.
///
/// Measured: `quang.tran@acme-internal.vn` came back as
/// `quang.tran@acme-internal . vn`.
fn emails(text: &str, already: &[Finding]) -> Vec<Finding> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    for (at, _) in text.char_indices().filter(|(_, c)| *c == '@') {
        let mut start = at;
        while start > 0 {
            let prev = bytes[start - 1] as char;
            if prev.is_ascii_alphanumeric() || "._%+-".contains(prev) {
                start -= 1;
            } else {
                break;
            }
        }
        if start == at {
            continue; // nothing in front of the @
        }

        // Forward through the domain. A space is tolerated only when it sits
        // against a dot -- `acme-internal . vn` is the measured damage, and
        // that rule stops the scan from swallowing the rest of the sentence
        // the way "any space followed by a letter" would.
        let mut end = at + 1;
        let mut last_solid = at + 1;
        let mut dot = false;
        while end < bytes.len() {
            let c = bytes[end] as char;
            if c.is_ascii_alphanumeric() || c == '-' {
                end += 1;
                last_solid = end;
            } else if c == '.' {
                dot = true;
                end += 1;
            } else if c == ' ' {
                let before_was_dot = end > 0 && bytes[end - 1] == b'.';
                let mut peek = end;
                while peek < bytes.len() && bytes[peek] == b' ' {
                    peek += 1;
                }
                let after_is_dot = peek < bytes.len() && bytes[peek] == b'.';
                if before_was_dot || after_is_dot {
                    end = peek;
                } else {
                    break;
                }
            } else {
                break;
            }
        }
        let end = last_solid;
        if !dot || end <= at + 2 {
            continue;
        }
        let span = start..end;
        if already.iter().any(|f| overlaps(&f.span, &span)) {
            continue;
        }
        out.push(Finding {
            kind: Kind::Email,
            text: text[span.clone()].to_string(),
            span,
        });
    }
    out
}

/// Card numbers, and only those that pass Luhn.
///
/// Without the check every sixteen-digit run is a card: order numbers, build
/// ids, a timestamp with the separators misread. Luhn is what keeps the false
/// positive count at zero, and zero is a measured property worth defending.
fn card_numbers(text: &str, already: &[Finding]) -> Vec<Finding> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if !(bytes[i] as char).is_ascii_digit() {
            i += 1;
            continue;
        }
        // Digits, optionally grouped by a single space or hyphen.
        let start = i;
        let mut digits = String::new();
        let mut end = i;
        while end < bytes.len() {
            let c = bytes[end] as char;
            if c.is_ascii_digit() {
                digits.push(c);
                end += 1;
            } else if (c == ' ' || c == '-')
                && end + 1 < bytes.len()
                && (bytes[end + 1] as char).is_ascii_digit()
                && !digits.is_empty()
            {
                end += 1;
            } else {
                break;
            }
        }
        i = end.max(start + 1);

        if (13..=19).contains(&digits.len()) && luhn(&digits) {
            let span = start..end;
            if !already.iter().any(|f| overlaps(&f.span, &span)) {
                out.push(Finding {
                    kind: Kind::CardNumber,
                    text: text[span.clone()].to_string(),
                    span,
                });
            }
        }
    }
    out
}

fn luhn(digits: &str) -> bool {
    let mut sum = 0u32;
    for (i, c) in digits.chars().rev().enumerate() {
        let mut d = c.to_digit(10).unwrap_or(0);
        if i % 2 == 1 {
            d *= 2;
            if d > 9 {
                d -= 9;
            }
        }
        sum += d;
    }
    sum.is_multiple_of(10)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The corpus that matters: strings this OCR engine actually produced,
    /// copied out of the S5.1 measurement rather than imagined.
    /// `docs/experiments/ocr-windows-media.md` has the run they came from.
    const AS_OCR_READ_IT: &[(&str, Kind)] = &[
        // `_` -> space, and `0` -> `e` in the body.
        ("GITHUB KEY=ghp 9fK2mQ7xR4tL8vN3bJ6hY1cW5dZeaE", Kind::Token),
        // `B` -> `8`, `l` -> `1`.
        (
            "OPENAI KEY=sk-proj-T381bkFJ9xQvNm2LpR8sKdW4aZ",
            Kind::ApiKey,
        ),
        ("AWS KEY=AKIAIOSFODNN7EXAMPLE", Kind::ApiKey),
        // Spaces sprinkled around the final dot.
        ("EMAIL KEY —quang.tran@acme-internal . vn", Kind::Email),
        // `B` -> `8` on Bearer, `I` -> `1` inside the token.
        (
            "BEARER KEY=8earer eyJhbGciOiJIUz11Ni1s1nR5cC161kpXVCJ9",
            Kind::Token,
        ),
    ];

    #[test]
    fn every_secret_ocr_actually_produced_is_found() {
        for (line, kind) in AS_OCR_READ_IT {
            let found = scan(line);
            assert!(
                found.iter().any(|f| f.kind == *kind),
                "missed the {} in {line:?} -- found {:?}",
                kind.label(),
                found.iter().map(|f| (f.kind, &f.text)).collect::<Vec<_>>()
            );
        }
    }

    /// The same strings as they appear in a file, which is the easy case and
    /// must not regress while the hard one is being handled.
    #[test]
    fn the_undamaged_originals_are_found_too() {
        for line in [
            "OPENAI_KEY=sk-proj-T3BlbkFJ9xQvNm2LpR8sKdW4aZ",
            "GITHUB_KEY=ghp_9fK2mQ7xR4tL8vN3bJ6hY1cW5dZ0aE",
            "AWS_KEY=AKIAIOSFODNN7EXAMPLE",
            "EMAIL_KEY=quang.tran@acme-internal.vn",
            "BEARER_KEY=Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9",
            "STRIPE_KEY=sk_live_51H8xQ2LmNpR4tV7wY0zA3bC6",
        ] {
            assert!(!scan(line).is_empty(), "found nothing in {line:?}");
        }
    }

    /// Zero false positives is a measured property of this scanner. These are
    /// the strings most likely to spend it.
    #[test]
    fn ordinary_screen_text_is_left_alone() {
        for line in [
            "Cach tinh gia theo so canh",
            "pricing.local/bang-gia",
            "framekeep-core 0.1.0  built for x64",
            "commit 43ace0ae17c78862fd1cb46d4137d5d1ff6d9432",
            "Showing 7 of 7 recordings",
            "2026-08-17 05:13:44  GET /api/videos 200 in 41ms",
            "C:\\Users\\ADMIN\\Videos\\framekeep-corpus\\switch.mp4",
            "1234567890123456789",          // long digit run, fails Luhn
            "order 4111111111111111234567", // digits around a card-like run
            "sk-short",                     // prefix without a body
            "ghp_tooshort",
        ] {
            let found = scan(line);
            assert!(found.is_empty(), "fired on {line:?}: {found:?}");
        }
    }

    #[test]
    fn a_card_number_needs_to_pass_luhn() {
        // A real test number from the card networks' published list.
        let hit = scan("card 4111 1111 1111 1111 on file");
        assert_eq!(hit.len(), 1, "{hit:?}");
        assert_eq!(hit[0].kind, Kind::CardNumber);

        // One digit changed: still sixteen digits, no longer a card.
        assert!(scan("card 4111 1111 1111 1112 on file").is_empty());
    }

    #[test]
    fn the_longest_rule_wins_so_one_key_is_reported_once() {
        let found = scan("sk-proj-T3BlbkFJ9xQvNm2LpR8sKdW4aZ");
        assert_eq!(found.len(), 1, "reported twice: {found:?}");
        assert!(found[0].text.starts_with("sk-proj-"));
    }

    /// S5.5, and a defect in the mockup that this is the fix for.
    #[test]
    fn a_finding_shows_which_key_without_showing_the_key() {
        let found = scan("sk-proj-T3BlbkFJ9xQvNm2LpR8sKdW4aZ");
        let masked = found[0].masked();
        assert_eq!(masked, "sk-••••••W4aZ");
        assert!(!masked.contains("T3Blbk"), "the body leaked: {masked}");
    }

    #[test]
    fn spans_point_at_what_matched() {
        let line = "before sk-proj-T3BlbkFJ9xQvNm2LpR8sKdW4aZ after";
        let found = scan(line);
        assert_eq!(&line[found[0].span.clone()], found[0].text);
    }

    #[test]
    fn several_secrets_on_one_line_all_come_back_in_order() {
        let line = "AKIAIOSFODNN7EXAMPLE and a@b.co and ghp_9fK2mQ7xR4tL8vN3bJ6hY1cW5";
        let found = scan(line);
        assert_eq!(found.len(), 3, "{found:?}");
        assert!(found.windows(2).all(|w| w[0].span.start <= w[1].span.start));
    }

    // --- S5.8: the words a person adds themselves ---------------------------

    fn words(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    /// The reason this is not a regex box.
    ///
    /// Somebody types the pattern the way it is written *on disk*, because that
    /// is the only way they have ever seen it. What OCR hands the scanner is
    /// the damaged spelling. A regex would match the first and be handed the
    /// second, fire never, and leave a person believing they were covered.
    #[test]
    fn a_word_typed_the_way_it_is_written_matches_the_way_ocr_read_it() {
        let custom = words(&["acme_key"]);
        // Every one of these is a measured OCR failure from the module header,
        // applied to the same typed word.
        for line in [
            "ACME_KEY = 9fK2mQ7x", // as written on disk
            "ACME KEY 9fK2mQ7x",   // underscore came back as a space
            "ACMEKEY 9fK2mQ7x",    // underscore vanished entirely
            "acme-key: 9fK2mQ7x",  // read as a hyphen
            "ACM0 K0Y 9fK2mQ7x",   // E->0, the measured pair, in both places
        ] {
            let found = scan_with(line, &custom);
            assert_eq!(found.len(), 1, "missed {line:?}");
            assert_eq!(found[0].kind, Kind::Custom);
        }

        // And the limit of it: only pairs the engine was *measured* confusing
        // are folded. `A`->`4` and `E`->`3` are leetspeak, not OCR damage, and
        // folding them would widen every pattern for nothing.
        assert!(
            scan_with("4CM3 K3Y 9fK2mQ7x", &custom).is_empty(),
            "folded a homoglyph nobody measured"
        );
    }

    /// A typed word does two jobs, and this is the one a plain literal misses.
    #[test]
    fn the_word_drags_along_whatever_is_stuck_to_it_and_stops_at_the_space() {
        let found = scan_with(
            "internal acme_51H8xKqR2mN and then some ordinary prose",
            &words(&["acme_"]),
        );
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(
            found[0].text, "acme_51H8xKqR2mN",
            "the key attached to the word has to come with it"
        );
        // `is_body` excludes the space, so a pattern can never eat a sentence.
        assert!(!found[0].text.contains(' '));
    }

    /// The other job: the run is not attached to anything, and the only thing
    /// saying it is a secret is the person's own label in front of it.
    /// The label is a word no built-in keyword covers, which is the whole
    /// reason somebody would type it in.
    #[test]
    fn a_run_after_someones_own_label_is_caught_and_wears_their_badge() {
        let found = scan_with("NHANVIEN 51H8xKqR7mNp2vXd9tE2", &words(&["nhanvien"]));
        // Both halves: the label itself, and the run that only the label marks
        // as a secret. Two findings rather than one is the honest count -- they
        // are two places on the frame and each gets its own box.
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(found[0].text, "NHANVIEN");
        assert_eq!(found[1].text, "51H8xKqR7mNp2vXd9tE2");
        assert!(
            found.iter().all(|f| f.kind == Kind::Custom),
            "calling somebody's own label an API key is this module guessing"
        );
    }

    /// Not every secret is high-entropy. A codename is a literal, and the whole
    /// point of typing it in is that no rule would ever infer it.
    #[test]
    fn a_phrase_with_nothing_attached_is_still_hidden() {
        let found = scan_with(
            "slides for Project Nightingale, Q3",
            &words(&["Project Nightingale"]),
        );
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].text, "Project Nightingale");
    }

    /// The guard that keeps a short word from blacking out a screen.
    #[test]
    fn a_pattern_only_fires_at_a_word_boundary() {
        let custom = words(&["acme"]);
        assert!(
            scan_with("panacme and panacmea", &custom).is_empty(),
            "fired inside a longer word"
        );
        assert_eq!(scan_with("the acme build", &custom).len(), 1);
    }

    #[test]
    fn a_pattern_too_short_to_be_safe_is_refused_with_a_reason() {
        let said = check_pattern("ab").expect_err("a two-letter pattern was accepted");
        assert!(said.contains("at least"), "{said}");
        // Length is judged after folding: `a_b` is three characters and two
        // letters, and it is the letters that decide what it covers.
        assert!(check_pattern("a_b").is_err());
        assert!(check_pattern("   ").is_err());
        assert_eq!(check_pattern("  acme  ").unwrap(), "acme");
        // And a pattern that slipped past validation is still refused here,
        // because `scan_with` is reachable from a CLI flag.
        assert!(scan_with("ab ab ab", &words(&["ab"])).is_empty());
    }

    /// Badge precedence. Somebody whose company is `acme` should not have every
    /// colleague's email address relabelled.
    #[test]
    fn a_shape_that_carries_its_own_meaning_keeps_its_own_badge() {
        let found = scan_with("quang.tran@acme-internal.vn", &words(&["acme"]));
        assert_eq!(found.len(), 1, "reported twice: {found:?}");
        assert_eq!(found[0].kind, Kind::Email);
    }

    /// The measured 83%/62%/8% belongs to the built-in rules. Somebody's own
    /// word may add findings; it may never take one away or relabel one, or a
    /// person could weaken the scanner by trying to strengthen it.
    #[test]
    fn a_persons_own_word_can_only_ever_add_to_what_the_rules_already_found() {
        let custom = words(&["acme", "nightingale"]);
        for line in [
            "sk-proj-T3BlbkFJ9xQvNm2LpR8sKdW4aZ",
            "AKIAIOSFODNN7EXAMPLE at acme",
            "card 4111 1111 1111 1111 on file",
            "quang.tran@acme-internal.vn",
        ] {
            let plain = scan(line);
            let with = scan_with(line, &custom);
            for f in &plain {
                assert!(
                    with.iter().any(|g| g.span == f.span && g.kind == f.kind),
                    "{line:?}: adding a pattern lost or relabelled {f:?}"
                );
            }
        }
    }
}

/// The scanner against every reading taken during the S5.1 measurement, rather
/// than against strings typed from memory -- which is how a scanner ends up
/// passing its tests and missing real keys.
///
/// Twelve frames of planted secrets (three font sizes, two themes, each read
/// clean and after a trip through h264) and five real screen recordings that
/// contain none. Fixtures come from `spike/s5-ocr-poc/dump-fixtures.py`; the
/// run is written up in `docs/experiments/ocr-windows-media.md`.
///
/// Both floors are asserted together on purpose. Recall and precision are
/// exactly the pair that gets traded by accident: loosening a pattern to catch
/// one more key is also how it starts firing on prose.
#[cfg(test)]
mod against_real_ocr {
    use super::*;
    use serde::Deserialize;

    /// The floor, and why it is not the prototype's 43.
    ///
    /// The prototype counted a kind as found when *its* pattern matched
    /// anywhere in the reading, so one match could be credited against a
    /// planted secret it had nothing to do with. This counts a planted secret
    /// only when a finding anchored to that secret came back. Different
    /// question, lower number, and the lower one is the true one.
    ///
    /// Measured 37/72 with the corrected accounting. Every remaining miss at
    /// 14 and 16px was checked by hand against the fixtures and is an **OCR
    /// omission, not a pattern gap** -- the GitHub line came back as
    /// `README.md GITHUB .gitignore AWS KEY=…` with the key simply absent, and
    /// the Stripe value vanished entirely at 14px. No pattern finds text that
    /// was never produced, and reaching for one would only spend precision.
    ///
    /// So this is a regression floor, not a target. Raising it means OCR got
    /// better; lowering it means something broke.
    const RECALL_FLOOR: usize = 36;
    const PLANTED_TOTAL: usize = 72;

    #[derive(Deserialize)]
    struct Reading {
        stem: String,
        size_px: u32,
        planted: Vec<Planted>,
        ocr_text: String,
    }

    #[derive(Deserialize)]
    struct Planted {
        kind: String,
    }

    #[derive(Deserialize)]
    struct Clean {
        name: String,
        ocr_text: String,
    }

    fn fixture(name: &str) -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("ocr")
            .join(name);
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("missing fixture {}: {e}", path.display()))
    }

    /// Whether a finding is *this* planted secret rather than merely the same
    /// badge.
    ///
    /// Matching on `Kind` alone was wrong and flattered nothing: `github` and
    /// `bearer` are both `Token`, so one finding was credited against whichever
    /// came first in the list and the other was reported missing. The anchors
    /// are the fragments that survive OCR damage -- deliberately short,
    /// because the body does not survive.
    fn is_the_planted_one(kind: &str, hit: &Finding) -> bool {
        let text = hit.text.to_ascii_lowercase();
        match kind {
            "openai" => {
                text.contains("proj") || (text.starts_with("sk-") && !text.contains("live"))
            }
            "github" => text.contains("ghp"),
            "aws" => text.contains("akia") || text.contains("ak1a") || text.contains("aria"),
            "email" => hit.kind == Kind::Email,
            "bearer" => text.contains("yj"),
            // The prefix is routinely gone; what is left is the body, and at
            // 14px even that is gone. `51h8` is its opening.
            "stripe" => text.contains("live") || text.contains("51h8"),
            other => panic!("fixture has an unplanned kind: {other}"),
        }
    }

    #[test]
    fn recall_is_at_least_what_the_prototype_measured() {
        let readings: Vec<Reading> = serde_json::from_str(&fixture("with-secrets.json")).unwrap();
        assert_eq!(readings.len(), 12, "the corpus lost readings");

        let mut found = 0;
        let mut by_size: std::collections::BTreeMap<u32, (usize, usize)> = Default::default();

        for reading in &readings {
            let hits = scan(&reading.ocr_text);
            // One finding can only account for one planted secret; matching by
            // badge rather than by text because OCR damages the body, and
            // covering a secret never required reading it.
            let mut remaining: Vec<&Finding> = hits.iter().collect();
            let mut here = 0;
            let mut missed = Vec::new();
            for planted in &reading.planted {
                match remaining
                    .iter()
                    .position(|h| is_the_planted_one(&planted.kind, h))
                {
                    Some(at) => {
                        remaining.remove(at);
                        here += 1;
                    }
                    None => missed.push(planted.kind.as_str()),
                }
            }
            found += here;
            let entry = by_size.entry(reading.size_px).or_insert((0, 0));
            entry.0 += here;
            entry.1 += reading.planted.len();
            println!(
                "{:24} {here}/{}  {}",
                reading.stem,
                reading.planted.len(),
                if missed.is_empty() {
                    String::new()
                } else {
                    format!("miss: {}", missed.join(","))
                }
            );
        }

        println!("\nby font size:");
        for (size, (hit, total)) in &by_size {
            println!(
                "  {size:>2}px  {hit:>2}/{total}  ({:.0}%)",
                100.0 * *hit as f64 / *total as f64
            );
        }
        println!("\ntotal {found}/{PLANTED_TOTAL}  (floor {RECALL_FLOOR})");

        assert!(
            found >= RECALL_FLOOR,
            "found {found}/{PLANTED_TOTAL}, below the floor of {RECALL_FLOOR} \
             measured on these same readings"
        );
    }

    #[test]
    fn nothing_fires_on_real_screen_recordings() {
        let clean: Vec<Clean> = serde_json::from_str(&fixture("no-secrets.json")).unwrap();
        assert_eq!(clean.len(), 5, "the clean corpus lost readings");

        let mut complaints = Vec::new();
        for reading in &clean {
            for hit in scan(&reading.ocr_text) {
                complaints.push(format!(
                    "{}: {} {:?}",
                    reading.name,
                    hit.kind.label(),
                    hit.text
                ));
            }
        }
        assert!(
            complaints.is_empty(),
            "invented {} secret(s) in recordings that have none:\n{}",
            complaints.len(),
            complaints.join("\n")
        );
    }

    #[test]
    fn no_finding_ever_prints_its_own_body() {
        let readings: Vec<Reading> = serde_json::from_str(&fixture("with-secrets.json")).unwrap();
        for reading in &readings {
            for hit in scan(&reading.ocr_text) {
                let masked = hit.masked();
                assert!(masked.contains('•'), "{masked} is not masked at all");
                if hit.text.chars().count() > 8 {
                    let middle: String = hit.text.chars().skip(3).take(4).collect();
                    assert!(!masked.contains(&middle), "the masked form leaks: {masked}");
                }
            }
        }
    }
}
