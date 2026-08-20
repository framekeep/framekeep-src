// The Review screen. S5.4 — the safety mechanism, not a convenience.
//
// What this file may render is bounded by what core emitted: masked values and
// boxes. The raw secrets never crossed the process boundary, so no bug here
// can leak one. What this file may DO is three Rust commands: read the review,
// save the ticks, approve — and approving always paints before the row moves.
//
// There is deliberately no "skip review" affordance anywhere in this screen,
// and none may ever be added: the measurement behind S5 says the scanner
// misses about one secret in six at readable sizes. The person is the second
// pass. Unticking every box is allowed — that is a decision, made in view of
// the frames — but nothing gets past this screen unseen.
//
// Routing: "" is the queue, "#review/<handle>" is this screen.

import { showReceipt, markReviewable, fileNameHtml, isTyping } from "./queue.js";

const { invoke, convertFileSrc } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

// Strings a person will read. Source of truth: _design_system/copy.md,
// "Duyệt redaction". Change there first.
const STR = {
  pager: (n, m) => `Frame ${n} of ${m}`,
  frameChip: (n) => `Frame ${n}`,
  items: (n) => `${n} item${n === 1 ? "" : "s"}`,
  unlocated: "Can't be hidden automatically — check this frame yourself.",
  unreadable: (k) =>
    `${k} frame${k === 1 ? "" : "s"} couldn't be read by the scanner — check them yourself.`,
  saved: "Saved. Your decisions will be here when you come back.",
  subtitleReviewed: "Approved. Adjust what's hidden and send again if you change your mind.",
  subtitleWaiting: "Nothing has been sent yet. Hide anything else you spot, then send.",
  approved: (n, m) => `${n} item${n === 1 ? "" : "s"} hidden on ${m} frame${m === 1 ? "" : "s"}.`,
  leftVisible: (k) => ` · ${k} left visible`,
  nothingHidden: "Nothing hidden — you approved the frames as they are.",
  // Approving unlocks a recording; it does not send one. Nothing said so, and
  // the button is called Send to chat, so the honest place to say it is the
  // sentence that follows the press.
  nextStep: " Now copy its prompt from the queue and ask your AI.",
  // Nav subtitle, two states. `Before sending` is copy.md's original and
  // stays the sentence for work that has not gone out; the second exists
  // because the entry no longer goes dark once everything is approved,
  // and "Before sending" would then be describing the wrong errand.
  navWaiting: "Before sending",
  navReviewed: "See what was hidden",
  fit: "Fit",
};

// Icons per badge label, matching core's `Kind::label` exactly. An unknown
// label still renders — with the generic mark — rather than dropping the row:
// a finding that vanishes from the list is the worst rendering bug available.
const KIND_ICONS = {
  "API key": '<path d="M10.8 2.5a4.7 4.7 0 0 0-4.6 5.8L2.5 12v3.5H6l.7-.7v-1.6h1.6l1-1h1.6l.6-1.7a4.7 4.7 0 1 0-.7-8Zm1.7 4.2a1.4 1.4 0 1 1 0-2.8 1.4 1.4 0 0 1 0 2.8Z"/>',
  Token: '<rect x="2.5" y="5" width="13" height="8" rx="2"/><path d="M5.5 9h.01M9 9h.01M12.5 9h.01"/>',
  "Email address": '<rect x="2.5" y="4" width="13" height="10" rx="2"/><path d="m3 5 6 5 6-5"/>',
  "Card number": '<rect x="2.5" y="4.5" width="13" height="9" rx="2"/><path d="M2.5 7.5h13M5 11h3"/>',
  "Private key": '<path d="M9 1.8 15 4v5c0 3.6-2.6 6.2-6 7.2C5.6 15.2 3 12.6 3 9V4l6-2.2Z"/><path d="M9 7v3M9 11.5h.01"/>',
  // S5.8. A tag, because this one is here for no reason but that somebody
  // attached their own word to it.
  "Custom pattern":
    '<path d="M9.2 2.5H15v5.8l-6.6 6.6a1.6 1.6 0 0 1-2.2 0L2.6 11.2a1.6 1.6 0 0 1 0-2.2L9.2 2.5Z"/><path d="M12.1 5.4h.01"/>',
  _: '<circle cx="9" cy="9" r="6.5"/><path d="M9 6v3.5M9 12h.01"/>',
};

const el = (id) => document.getElementById(id);

const state = {
  handle: null,
  data: null,
  ticks: [],
  /// Regions the person drew, per frame, in image pixels. Drawing one IS the
  /// decision -- no tick; deleting the box is the undo.
  extras: [],
  frame: 0,
  zoom: null, // null = fit; otherwise a factor, 0.5 .. 3
};

// --- routing ---------------------------------------------------------------

function route() {
  const m = location.hash.match(/^#review\/([0-9a-f]{16})$/);
  if (m) open(m[1]);
  else close();
}

async function open(handle) {
  state.handle = handle;
  try {
    state.data = await invoke("review_data", { handle });
  } catch (e) {
    location.hash = "";
    showReceipt(String(e), "screen-review");
    return;
  }
  // The screen serves two states: waiting on the person, and already
  // approved — reopened to look again or change their mind. Anything else
  // (still processing, errored, purged) has nothing to review.
  if (state.data.stage !== "needs_review" && state.data.stage !== "ready") {
    location.hash = "";
    return;
  }
  el("rv-subtitle").textContent =
    state.data.stage === "ready" ? STR.subtitleReviewed : STR.subtitleWaiting;

  state.ticks = state.data.frames.map((f) => f.detections.map((d) => d.approved));
  state.extras = state.data.extras ?? state.data.frames.map(() => []);
  state.frame = Math.max(0, state.data.frames.findIndex((f) => f.detections.length > 0));
  state.zoom = null;

  el("screen-queue").hidden = true;
  el("screen-review").hidden = false;
  setNav(true);
  render();
}

function close() {
  state.handle = null;
  state.data = null;
  el("screen-review").hidden = true;
  el("screen-queue").hidden = false;
  setNav(false);
}

function setNav(onReview) {
  const queueNav = el("nav-queue");
  const reviewNav = el("nav-review");
  queueNav.classList.toggle("is-active", !onReview);
  reviewNav.classList.toggle("is-active", onReview);
  if (onReview) {
    queueNav.removeAttribute("aria-current");
    reviewNav.setAttribute("aria-current", "page");
  } else {
    reviewNav.removeAttribute("aria-current");
    queueNav.setAttribute("aria-current", "page");
  }
}

// --- formatting (same rules as the queue) ----------------------------------

function fmtDuration(ms) {
  if (ms == null) return "";
  const s = Math.round(ms / 1000);
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
}

function fmtTime(pts) {
  if (pts == null) return "";
  const s = Math.floor(pts);
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
}

const RATIOS = [[16, 9], [16, 10], [4, 3], [3, 2], [21, 9], [5, 4], [1, 1], [9, 16]];

function fmtMeta(d) {
  const parts = [];
  if (d.duration_ms != null) parts.push(fmtDuration(d.duration_ms));
  if (d.width && d.height) parts.push(`${d.width}×${d.height}`);
  if (d.frame_count != null) parts.push(`${d.frame_count} key frames`);
  if (d.width && d.height) {
    const actual = d.width / d.height;
    const near = RATIOS.find(([w, h]) => Math.abs(actual - w / h) / (w / h) < 0.02);
    if (near) parts.push(`${near[0]}:${near[1]}`);
  }
  return parts.join(" · ");
}

function escapeHtml(s) {
  return String(s).replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);
}

// --- rendering -------------------------------------------------------------

function render() {
  const d = state.data;
  el("rv-name").innerHTML = fileNameHtml(d.name);
  el("rv-name").title = d.name;
  el("rv-meta").textContent = fmtMeta(d);
  el("rv-frames-pill").textContent = `${d.frames.length} key frames`;

  renderFilmstrip();
  renderPreview();
  renderDetections();
}

function renderFilmstrip() {
  const d = state.data;
  el("rv-filmstrip").innerHTML = d.frames
    .map((f, i) => {
      const selected = i === state.frame ? " is-selected" : "";
      const flagged = f.detections.length > 0 ? '<span class="strip-dot" aria-hidden="true"></span>' : "";
      return `<button class="strip-thumb${selected}" role="option" aria-selected="${i === state.frame}" data-frame="${i}">
        <img src="${convertFileSrc(f.file)}" alt="" loading="lazy" draggable="false" />
        ${flagged}
        <span class="strip-time">${fmtTime(f.pts_time)}</span>
      </button>`;
    })
    .join("");
  const sel = el("rv-filmstrip").querySelector(".is-selected");
  if (sel) sel.scrollIntoView({ block: "nearest", inline: "nearest" });
}

function renderPreview() {
  const frame = state.data.frames[state.frame];
  el("rv-time").textContent = fmtTime(frame.pts_time);
  el("rv-pager").textContent = STR.pager(state.frame + 1, state.data.frames.length);
  el("rv-zoom-level").textContent = state.zoom == null ? STR.fit : `${Math.round(state.zoom * 100)}%`;

  const img = el("rv-frame");
  img.onload = layoutPreview;
  img.src = convertFileSrc(frame.file);

  // Overlays: solid amber for a box that will be painted, dashed and quiet for
  // one the person chose to leave visible. Both stay on screen — the review is
  // exactly the act of looking at these.
  const w = state.data.width || 1;
  const h = state.data.height || 1;
  const pct = (b) =>
    `left:${(100 * b.x) / w}%;top:${(100 * b.y) / h}%;width:${(100 * b.w) / w}%;height:${(100 * b.h) / h}%`;
  const detected = frame.detections.flatMap((det, di) =>
    det.boxes.map(
      (b) => `<span class="ov${state.ticks[state.frame][di] ? "" : " ov-off"}" style="${pct(b)}"></span>`
    )
  );
  const drawn = (state.extras[state.frame] ?? []).map(
    (b, i) =>
      `<span class="ov ov-extra" style="${pct(b)}"><button class="ov-x" data-extra="${i}" aria-label="Remove this area">×</button></span>`
  );
  el("rv-overlays").innerHTML = detected.join("") + drawn.join("");
  layoutPreview();
}

// Fit is a computed factor, not a CSS trick: the overlay percentages need the
// stage to be exactly the image's aspect, so the stage gets explicit pixels.
function layoutPreview() {
  const img = el("rv-frame");
  const stage = el("rv-stage");
  const scroll = el("rv-preview-scroll");
  if (!img.naturalWidth) return;

  const factor =
    state.zoom == null
      ? Math.min(
          (scroll.clientWidth - 24) / img.naturalWidth,
          (scroll.clientHeight - 24) / img.naturalHeight
        )
      : state.zoom;

  stage.style.width = `${img.naturalWidth * factor}px`;
  stage.style.height = `${img.naturalHeight * factor}px`;
}

function renderDetections() {
  const d = state.data;
  const total = d.frames.reduce((n, f) => n + f.detections.length, 0);
  el("rv-count").textContent = STR.items(total);

  const anyTicked = state.ticks.some((f) => f.some(Boolean));
  el("rv-deselect").disabled = !anyTicked;

  el("rv-detections").innerHTML = d.frames
    .flatMap((frame, fi) =>
      frame.detections.map((det, di) => {
        const ticked = state.ticks[fi][di];
        const icon = KIND_ICONS[det.kind] ?? KIND_ICONS._;
        // An unlocated finding cannot be painted, so its checkbox would be a
        // false comfort: shown ticked-and-locked, with the warning carrying
        // the truth instead.
        const box = det.located
          ? `<input type="checkbox" class="det-tick" data-frame="${fi}" data-det="${di}" ${ticked ? "checked" : ""} aria-label="Hide this ${escapeHtml(det.kind)}" />`
          : `<span class="det-warn-icon" aria-hidden="true"><svg viewBox="0 0 18 18"><path d="M9 2 16.5 15h-15L9 2Z"/><path d="M9 7.5v3M9 12.8h.01"/></svg></span>`;
        return `<div class="det${ticked && det.located ? "" : " det-off"}${det.located ? "" : " det-unlocated"}" data-frame="${fi}">
          ${box}
          <span class="det-icon" aria-hidden="true"><svg viewBox="0 0 18 18">${icon}</svg></span>
          <span class="det-text">
            <span class="det-kind">${escapeHtml(det.kind)}</span>
            <span class="det-value">${escapeHtml(det.masked)}</span>
            ${det.located ? "" : `<span class="det-warn">${STR.unlocated}</span>`}
          </span>
          <span class="det-where">
            <span class="chip-time">${fmtTime(frame.pts_time)}</span>
            <button class="det-jump" data-frame="${fi}" aria-label="Show ${STR.frameChip(fi + 1)}">${STR.frameChip(fi + 1)}
              <svg viewBox="0 0 18 18" aria-hidden="true"><path d="m7 3.5 5.5 5.5L7 14.5"/></svg>
            </button>
          </span>
        </div>`;
      })
    )
    .join("");

  const unreadable = el("rv-unreadable");
  unreadable.hidden = !d.unreadable_frames;
  if (d.unreadable_frames) unreadable.textContent = STR.unreadable(d.unreadable_frames);
}

// --- interactions ----------------------------------------------------------

function selectFrame(i) {
  state.frame = Math.max(0, Math.min(state.data.frames.length - 1, i));
  renderFilmstrip();
  renderPreview();
}

el("rv-filmstrip").addEventListener("click", (e) => {
  const thumb = e.target.closest(".strip-thumb");
  if (thumb) selectFrame(Number(thumb.dataset.frame));
});
el("rv-strip-prev").addEventListener("click", () => selectFrame(state.frame - 1));
el("rv-strip-next").addEventListener("click", () => selectFrame(state.frame + 1));
el("rv-prev").addEventListener("click", () => selectFrame(state.frame - 1));
el("rv-next").addEventListener("click", () => selectFrame(state.frame + 1));

el("rv-detections").addEventListener("change", (e) => {
  const tick = e.target.closest(".det-tick");
  if (!tick) return;
  state.ticks[Number(tick.dataset.frame)][Number(tick.dataset.det)] = tick.checked;
  renderPreview();
  renderDetections();
});
el("rv-detections").addEventListener("click", (e) => {
  const jump = e.target.closest(".det-jump");
  if (jump) selectFrame(Number(jump.dataset.frame));
});
el("rv-deselect").addEventListener("click", () => {
  state.ticks = state.ticks.map((f) => f.map(() => false));
  renderPreview();
  renderDetections();
});

// Zoom: fit by default; steps of 25% between 50% and 300% once touched.
function setZoom(z) {
  state.zoom = z;
  el("rv-zoom-level").textContent = z == null ? STR.fit : `${Math.round(z * 100)}%`;
  layoutPreview();
}
function currentFactor() {
  if (state.zoom != null) return state.zoom;
  const img = el("rv-frame");
  return img.naturalWidth ? el("rv-stage").clientWidth / img.naturalWidth : 1;
}
el("rv-zoom-in").addEventListener("click", () => setZoom(Math.min(3, currentFactor() + 0.25)));
el("rv-zoom-out").addEventListener("click", () => setZoom(Math.max(0.5, currentFactor() - 0.25)));
el("rv-zoom-fit").addEventListener("click", () => setZoom(null));
window.addEventListener("resize", layoutPreview);

// --- leaving the screen ----------------------------------------------------

el("rv-back").addEventListener("click", () => (location.hash = ""));
el("rv-cancel").addEventListener("click", () => (location.hash = ""));

el("rv-save").addEventListener("click", async () => {
  try {
    await invoke("save_review", { handle: state.handle, ticks: state.ticks, extras: state.extras });
    showReceipt(STR.saved, "screen-review");
  } catch (e) {
    showReceipt(String(e), "screen-review");
  }
});

el("rv-send").addEventListener("click", async () => {
  const send = el("rv-send");
  send.disabled = true;
  try {
    const done = await invoke("approve_recording", { handle: state.handle, ticks: state.ticks, extras: state.extras });
    let message =
      done.masks_applied > 0
        ? STR.approved(done.masks_applied, done.frames_redacted)
        : STR.nothingHidden;
    if (done.left_visible > 0) message += STR.leftVisible(done.left_visible);
    message += STR.nextStep;
    location.hash = "";
    showReceipt(message);
  } catch (e) {
    showReceipt(String(e), "screen-review");
  } finally {
    send.disabled = false;
  }
});

document.addEventListener("keydown", (e) => {
  if (!state.handle || isTyping(e)) return;
  if (e.key === "Escape") location.hash = "";
  if (e.key === "ArrowLeft") selectFrame(state.frame - 1);
  if (e.key === "ArrowRight") selectFrame(state.frame + 1);
});

// --- drag to hide more (S5.6) ----------------------------------------------
//
// A rubber band on the preview becomes a region in IMAGE pixels -- divided by
// the current scale, so it means the same thing at Fit and at 300% -- and goes
// into state.extras for its frame. It ships to Rust with save and send, lands
// in review.json (never scan.json: that file is evidence), and apply() paints
// it with no tick asked: drawing it was the decision. The × on the box is the
// undo.

let band = null; // { x0, y0 } in stage px while dragging

function stagePoint(e) {
  const r = el("rv-stage").getBoundingClientRect();
  return {
    x: Math.max(0, Math.min(e.clientX - r.left, r.width)),
    y: Math.max(0, Math.min(e.clientY - r.top, r.height)),
  };
}

el("rv-stage").addEventListener("mousedown", (e) => {
  if (e.button !== 0 || e.target.closest(".ov-x")) return;
  band = stagePoint(e);
  const ghost = document.createElement("span");
  ghost.className = "ov ov-ghost";
  ghost.id = "rv-ghost";
  el("rv-overlays").append(ghost);
  e.preventDefault();
});

window.addEventListener("mousemove", (e) => {
  if (!band) return;
  const p = stagePoint(e);
  const ghost = el("rv-ghost");
  if (!ghost) return;
  const stage = el("rv-stage");
  ghost.style.left = `${(100 * Math.min(band.x, p.x)) / stage.clientWidth}%`;
  ghost.style.top = `${(100 * Math.min(band.y, p.y)) / stage.clientHeight}%`;
  ghost.style.width = `${(100 * Math.abs(p.x - band.x)) / stage.clientWidth}%`;
  ghost.style.height = `${(100 * Math.abs(p.y - band.y)) / stage.clientHeight}%`;
});

window.addEventListener("mouseup", (e) => {
  if (!band) return;
  const start = band;
  band = null;
  el("rv-ghost")?.remove();
  const p = stagePoint(e);
  const stage = el("rv-stage");
  const wpx = Math.abs(p.x - start.x);
  const hpx = Math.abs(p.y - start.y);
  // Under 6 stage pixels is a click, not a drawing.
  if (wpx < 6 || hpx < 6) return;
  const scale = stage.clientWidth / (state.data.width || 1);
  state.extras[state.frame].push({
    x: Math.min(start.x, p.x) / scale,
    y: Math.min(start.y, p.y) / scale,
    w: wpx / scale,
    h: hpx / scale,
  });
  renderPreview();
});

el("rv-overlays").addEventListener("click", (e) => {
  const x = e.target.closest(".ov-x");
  if (!x) return;
  state.extras[state.frame].splice(Number(x.dataset.extra), 1);
  renderPreview();
});

// The nav entry is a way IN, not a badge for unread work. It used to go grey
// the moment the last recording was approved -- including while the person was
// standing on the review screen, so the entry for the screen you are looking at
// was dimmed as unavailable. Reported from real use, 18/08.
//
// It now stays live for anything queue.js says has a review to show, and only
// the subtitle changes: something waiting is a different errand from going back
// to look at what you hid.
let reviewTarget = null;
markReviewable((handle, { waiting } = {}) => {
  reviewTarget = handle;
  const nav = el("nav-review");
  if (handle) nav.removeAttribute("aria-disabled");
  else nav.setAttribute("aria-disabled", "true");
  el("nav-review-sub").textContent = waiting ? STR.navWaiting : STR.navReviewed;
});
el("nav-review").addEventListener("click", () => {
  if (reviewTarget) location.hash = `review/${reviewTarget}`;
});
el("nav-queue").addEventListener("click", () => (location.hash = ""));

// A row that vanishes underneath an open review — purged, expired — must not
// leave a stale screen up. A stage change alone is fine: approving from this
// very screen flips the row to ready, and reopened ready rows live here.
listen("queue-changed", () => {
  if (state.handle) {
    const h = state.handle;
    invoke("review_data", { handle: h }).then(
      () => {},
      () => {
        if (state.handle === h) location.hash = "";
      }
    );
  }
});

window.addEventListener("hashchange", route);
route();
