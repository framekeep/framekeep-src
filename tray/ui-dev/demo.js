// The standard demo dataset — screenshots.md §2, one set, matching the mockup.
//
// Every value here is deliberately fake: keys carry EXAMPLE in the body,
// mail lives on example.com (RFC 2606), file names are the dev use-cases the
// spec lists. Sensitive values appear ONLY in masked form — the screenshots
// must model the product's own promise.
//
// The frozen clock: rows are timed against 2026-08-12T10:30 so "Today,
// 10:24 AM" comes out identical on every run (the harness sets __DEMO_NOW__).
//
// One deliberate change from the spec's table, dated 18/08: the row that is
// mid-"Scanning for secrets" shows "—" for sensitive items, not "1". The
// count is what the scan produces; a count next to a still-running scan is a
// state the real pipeline cannot reach, and demo data may not show states
// the product cannot.

export const DEMO_NOW = "2026-08-12T10:30:00";

const HANDLE = "feedfacecafe0042";

// Boxes measured off acme-frame.html via its ?measure mode — never guessed.
// Re-measure if that file's layout changes (shoot.ps1 prints the command).
const KEY_BOX = { x: 563, y: 284, w: 228, h: 28 };
const EMAIL_BOX = { x: 613, y: 285, w: 162, h: 28 };

const T = (iso) => Math.floor(new Date(iso).getTime() / 1000);

function row(name, created, stage, secs, frames, found, thumb) {
  return {
    handle: HANDLE.slice(0, 12) + String(Math.abs(hash(name))).padStart(4, "0").slice(0, 4),
    name,
    stage,
    created_at: T(created),
    duration_ms: secs == null ? null : secs * 1000,
    width: 1920,
    height: 1080,
    frames_found: frames,
    sensitive_items: found,
    awaiting_review: stage === "needs_review",
    source_is_ours: false,
    expires_at: T(created) + 7 * 86_400,
    source_will_be_deleted: false,
    error: null,
    thumbnail: thumb,
  };
}

function hash(s) {
  let h = 0;
  for (const c of s) h = (h * 31 + c.charCodeAt(0)) | 0;
  return h;
}

export function demoQueue(fixtures) {
  const keys = `${fixtures}acme-keys.png`;
  const team = `${fixtures}acme-team.png`;
  const f1 = `${fixtures}frame-00001.webp`;
  const f2 = `${fixtures}frame-00002.webp`;
  const rows = [
    row("fix-payment-flow.mp4", "2026-08-12T10:24:00", "ready", 42, 11, 2, keys),
    row("add-dark-mode-toggle.mp4", "2026-08-12T09:58:00", "transcribing", 78, 18, null, f1),
    row("checkout-error-case.mp4", "2026-08-12T09:12:00", "needs_review", 31, 7, 3, team),
    row("refactor-auth-hook.mp4", "2026-08-11T18:43:00", "ready", 123, 23, 0, f2),
    row("update-pricing-page.mp4", "2026-08-11T16:21:00", "scanning", 55, 13, null, keys),
    row("bug-navbar-overlap.mp4", "2026-08-11T14:10:00", "ready", 28, 6, 0, f1),
    row("onboarding-walkthrough.mp4", "2026-08-10T22:33:00", "ready", 102, 20, 4, team),
  ];
  // The review deep-link uses the first row's handle.
  rows[0].handle = HANDLE;
  return rows;
}

export function demoReview(fixtures) {
  const keys = `${fixtures}acme-keys.png`;
  const team = `${fixtures}acme-team.png`;
  // Real scene detection is never evenly spaced; neither is this.
  const pts = [0, 2, 6, 10, 14, 18, 22, 26, 31, 36, 40];
  return {
    handle: HANDLE,
    name: "fix-payment-flow.mp4",
    stage: "needs_review",
    duration_ms: 42_000,
    width: 1920,
    height: 1080,
    frame_count: 11,
    engine: { available: true, language: "en-US" },
    unreadable_frames: 0,
    frames: pts.map((t, i) => ({
      file: i === 1 ? team : keys,
      pts_time: t,
      error: null,
      detections:
        i === 0
          ? [{ kind: "API key", masked: "sk-••••••4f2a", boxes: [KEY_BOX], located: true, approved: true }]
          : i === 1
            ? [{ kind: "Email address", masked: "al••@example.com", boxes: [EMAIL_BOX], located: true, approved: true }]
            : [],
    })),
  };
}

export const DEMO_HANDLE = HANDLE;
