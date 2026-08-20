// The Queue screen. Renders rows from queue.db and never anything else --
// what this file may do is bounded by the Rust commands and two events.
//
// The clipboard is not readable from here, by design and by test: pasting
// asks Rust (`paste_from_window`), whose gesture type exists only because a
// person pressed something. See src/clipboard.rs.
//
// The review screen (review.js) plugs in through two exports at the bottom
// and the location hash; this file stays the only reader of the queue.

const { invoke, convertFileSrc } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

// Strings a person will read. Source of truth: _design_system/copy.md,
// "Hàng đợi". Change there first.
const STR = {
  recordings: (n) => `${n} recording${n === 1 ? "" : "s"}`,
  showing: (n, total) => `Showing ${n} of ${total} recording${total === 1 ? "" : "s"}`,
  frames: (n) => `${n} key frames`,
  // `found`, not `hidden`: this column shows on rows nobody has reviewed yet,
  // where nothing IS hidden -- and after review the hidden count can be lower
  // than the found count if the person unticked. One word true in every state.
  found: (n) => `${n} found`,
  none: "—", // — : nothing to show, distinct from 0-and-counting
  deletesIn: (days) => `Deletes in ${days} day${days === 1 ? "" : "s"}`,
  stages: {
    extracting_frames: "Extracting frames",
    transcribing: "Transcribing",
    scanning: "Scanning for secrets",
    needs_review: "Needs review",
    ready: "Ready",
    error: "Error",
  },
  tabs: { all: "All", ready: "Ready", processing: "Processing", needs_review: "Needs review", error: "Error" },
  promptCopied: "Copied. Paste it into Claude Code, Cursor, VS Code or Codex.",
  // Two labels, because a dimmed button with no reason is the thing this app
  // spent a day removing. The locked one says what to do, not just that it is
  // locked.
  copyReady: (name) => `Copy the prompt for ${name}`,
  copyLocked: (name) => `Approve ${name} first — then you can copy its prompt`,
  copyLockedReason: "Review and approve this recording first — nothing is readable until you do.",
};

const PROCESSING = ["extracting_frames", "transcribing", "scanning"];

const state = {
  rows: [],
  tab: "all",
  search: "",
  hideCompleted: localStorage.getItem("hideCompleted") === "1",
  /// any | found | clean | unscanned. Deliberately the only axis here: the
  /// tabs already filter by stage and the box already searches by name, and a
  /// menu that repeats them is a second way to do one thing.
  ///
  /// `unscanned` exists because NULL and 0 are different facts everywhere else
  /// in this product -- "nobody looked" is not "looked and found nothing" --
  /// and a filter that merged them here would be the one place that lies.
  found: "any",
};

// --- data ------------------------------------------------------------------

async function load() {
  try {
    const reply = await invoke("queue_snapshot");
    state.rows = reply.items ?? [];
  } catch (e) {
    // The table shows what the queue answered; a queue that cannot answer
    // shows as empty plus a receipt line, not as a stack trace.
    state.rows = [];
    showReceipt(String(e));
  }
  render();
}

// Stage changes arrive in bursts (probe, frames, transcribe); repaint once.
let reloadTimer;
function reloadSoon() {
  clearTimeout(reloadTimer);
  reloadTimer = setTimeout(load, 80);
}

// --- formatting ------------------------------------------------------------

function fmtDuration(ms) {
  if (ms == null) return STR.none;
  const s = Math.round(ms / 1000);
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
}

// The ratio names the shape only when a person would name it the same way.
// A raw gcd on a 1544×816 window recording says "193:102" -- true, useless,
// and it wrapped the meta line. Within 2% of a household ratio we say the
// household name; otherwise the resolution stands alone.
const RATIOS = [
  [16, 9],
  [16, 10],
  [4, 3],
  [3, 2],
  [21, 9],
  [5, 4],
  [1, 1],
  [9, 16],
];

function fmtResolution(row) {
  if (!row.width || !row.height) return "";
  const actual = row.width / row.height;
  const near = RATIOS.find(([w, h]) => Math.abs(actual - w / h) / (w / h) < 0.02);
  const ratio = near ? ` · ${near[0]}:${near[1]}` : "";
  return `${row.width}×${row.height}${ratio}`;
}

// Demo mode (screenshots.md) freezes the clock so "Today, 10:24 AM" is the
// same on every run. Nothing sets __DEMO_NOW__ in the shipped app.
const now = () => (window.__DEMO_NOW__ ? new Date(window.__DEMO_NOW__) : new Date());

function fmtCreated(unix) {
  const then = new Date(unix * 1000);
  const time = then.toLocaleTimeString(undefined, { hour: "numeric", minute: "2-digit" });
  const startOfDay = (d) => new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
  const days = Math.round((startOfDay(now()) - startOfDay(then)) / 86_400_000);
  if (days === 0) return `Today, ${time}`;
  if (days === 1) return `Yesterday, ${time}`;
  return `${then.toLocaleDateString(undefined, { month: "short", day: "numeric" })}, ${time}`;
}

// --- rendering -------------------------------------------------------------

function stageChip(row) {
  const label = STR.stages[row.stage] ?? row.stage;
  if (PROCESSING.includes(row.stage)) {
    return `<span class="chip chip-processing"><span class="spin" aria-hidden="true"></span>${label}</span>`;
  }
  if (row.stage === "ready") {
    return `<span class="chip chip-ready"><span class="status-dot" style="background:var(--success)" aria-hidden="true"></span>${label}</span>`;
  }
  if (row.stage === "needs_review") {
    return `<span class="chip chip-review"><span class="status-dot" style="background:var(--warning-dot)" aria-hidden="true"></span>${label}</span>`;
  }
  const title = row.error ? ` title="${escapeHtml(row.error)}"` : "";
  return `<span class="chip chip-error"${title}><span class="status-dot" style="background:var(--error)" aria-hidden="true"></span>${label}</span>`;
}

function thumb(row) {
  if (row.thumbnail) {
    return `<img class="thumb" alt="" loading="lazy" src="${convertFileSrc(row.thumbnail)}" />`;
  }
  return `<span class="thumb is-blank" aria-hidden="true"><svg viewBox="0 0 18 18"><rect x="2.5" y="4" width="13" height="10" rx="2"/><path d="m2.5 11 3.5-3 3 2.5 3-2.5 3.5 3"/></svg></span>`;
}

function rowHtml(row) {
  const sensitive = row.sensitive_items > 0 ? STR.found(row.sensitive_items) : STR.none;
  const frames = row.frames_found != null ? STR.frames(row.frames_found) : STR.none;

  // L5c: the countdown sits on the row, and only on rows whose file Framekeep
  // itself will delete. A recording the user pointed at never shows one.
  let countdown = "";
  if (row.source_will_be_deleted && row.expires_at) {
    const days = Math.max(1, Math.ceil((row.expires_at * 1000 - Date.now()) / 86_400_000));
    countdown = `<span class="cell-sub">${STR.deletesIn(days)}</span>`;
  }

  const reviewable = isReviewable(row) ? " is-reviewable" : "";
  return `<div class="row${reviewable}" role="row" data-handle="${row.handle}">
    <span class="rec">${thumb(row)}
      <span class="rec-text">
        <span class="rec-name" title="${escapeHtml(row.name)}">${fileNameHtml(row.name)}</span>
        <span class="rec-meta">${fmtResolution(row)}</span>
      </span>
    </span>
    <span class="cell">${fmtDuration(row.duration_ms)}</span>
    <span class="cell is-quiet">${frames}</span>
    <span>${stageChip(row)}</span>
    <span class="cell is-quiet">${sensitive}</span>
    <span class="created"><span class="cell is-quiet">${fmtCreated(row.created_at)}</span>${countdown}</span>
    <button class="copy-prompt" data-handle="${row.handle}"
      ${row.stage === "ready" ? "" : 'aria-disabled="true"'}
      aria-label="${escapeHtml(row.stage === "ready" ? STR.copyReady(row.name) : STR.copyLocked(row.name))}"
      title="${escapeHtml(row.stage === "ready" ? STR.copyReady(row.name) : STR.copyLocked(row.name))}">
      <svg viewBox="0 0 18 18" aria-hidden="true"><rect x="6" y="6" width="9.5" height="9.5" rx="2"/><path d="M12 6V4.5a2 2 0 0 0-2-2H4.5a2 2 0 0 0-2 2V10a2 2 0 0 0 2 2H6"/></svg>
    </button>
    <button class="dots" aria-label="More actions for ${escapeHtml(row.name)}" aria-haspopup="menu">
      <svg viewBox="0 0 18 18"><circle cx="4" cy="9" r="1.4"/><circle cx="9" cy="9" r="1.4"/><circle cx="14" cy="9" r="1.4"/></svg>
    </button>
  </div>`;
}

function visibleRows() {
  return state.rows.filter((row) => {
    if (state.tab === "ready" && row.stage !== "ready") return false;
    if (state.tab === "processing" && !PROCESSING.includes(row.stage)) return false;
    if (state.tab === "needs_review" && row.stage !== "needs_review") return false;
    if (state.tab === "error" && row.stage !== "error") return false;
    if (state.hideCompleted && row.stage === "ready") return false;
    // Filename only. Searching what was said needs a transcript index, and
    // that index is the competitor's memory.db -- refused by name in
    // docs/spec-s3-retention.md.
    if (state.search && !row.name.toLowerCase().includes(state.search)) return false;
    if (state.found === "found" && !(row.sensitive_items > 0)) return false;
    if (state.found === "clean" && row.sensitive_items !== 0) return false;
    if (state.found === "unscanned" && row.sensitive_items != null) return false;
    return true;
  });
}

function render() {
  const rows = visibleRows();
  const counts = {
    all: state.rows.length,
    ready: state.rows.filter((r) => r.stage === "ready").length,
    processing: state.rows.filter((r) => PROCESSING.includes(r.stage)).length,
    needs_review: state.rows.filter((r) => r.stage === "needs_review").length,
    error: state.rows.filter((r) => r.stage === "error").length,
  };

  document.getElementById("nav-queue-count").textContent = STR.recordings(counts.all);

  const dot = { ready: "var(--success)", processing: "var(--warning-dot)", needs_review: "var(--warning-dot)", error: "var(--error)" };
  document.getElementById("tabs").innerHTML = Object.keys(STR.tabs)
    .map((key) => {
      const active = state.tab === key ? " is-active" : "";
      const marker = key === "all" ? "" : `<span class="status-dot" style="background:${dot[key]}" aria-hidden="true"></span>`;
      const count = key === "all" ? "" : `<span class="count">${counts[key]}</span>`;
      return `<button class="tab${active}" role="tab" aria-selected="${state.tab === key}" data-tab="${key}">${marker}${STR.tabs[key]}${count}</button>`;
    })
    .join("");

  document.getElementById("rows").innerHTML = rows.map(rowHtml).join("");
  document.getElementById("empty").hidden = state.rows.length > 0;
  document.getElementById("showing").textContent = STR.showing(rows.length, state.rows.length);

  // Tell the review side what it can open. Newest first is the list's own
  // order, so the first match is the one the nav entry should go to.
  //
  // Waiting rows win over reviewed ones no matter how new: the nav says
  // "Before sending", and something that has not been sent is the thing a
  // person clicking it means. Only when nothing is waiting does it fall back
  // to the newest reviewed row, so the entry stays a way in rather than going
  // dark the moment the last recording is approved.
  const waiting = state.rows.find((r) => r.stage === "needs_review");
  const reopenable = waiting ?? state.rows.find(isReviewable);
  reviewableListener?.(reopenable ? reopenable.handle : null, {
    waiting: waiting != null,
  });
}

/// Whether this row has a review screen to show.
///
/// One definition, used by the row and by the nav entry, because they had two
/// and disagreed. The row let you reopen an approved recording -- the owner's
/// first real session asked for that, and the comment here said "approving
/// must not lock the door behind you" -- while the nav entry, the control
/// actually labelled Review, only ever pointed at unreviewed rows and went
/// grey as soon as the last one was approved. The door was open and the door
/// handle was not.
///
/// A ready row with NULL findings has no scan record, so there is nothing to
/// show; it stays inert rather than opening an empty screen.
export function isReviewable(row) {
  return row.stage === "needs_review" || (row.stage === "ready" && row.sensitive_items != null);
}

function escapeHtml(s) {
  return String(s).replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);
}

/// A file name as stem + extension markup, so CSS can ellipsize the stem and
/// keep the extension: `fix-paymen….mp4`, never `fix-payment-flow.m…`. Used by
/// the queue rows and the review's file card -- one splitter, or the two
/// screens would eventually disagree about what counts as an extension.
export function fileNameHtml(name) {
  const at = name.lastIndexOf(".");
  // A leading dot or no dot is all stem: `.env` has nothing to preserve.
  if (at <= 0) return `<span class="stem">${escapeHtml(name)}</span>`;
  return `<span class="stem">${escapeHtml(name.slice(0, at))}</span><span class="ext">${escapeHtml(name.slice(at))}</span>`;
}

// --- receipt ---------------------------------------------------------------

let receiptTimer;
/// Whether the keystroke belongs to something the person is typing into.
///
/// Every shortcut below is registered on `document`, which means it also fires
/// while the caret sits in a text box -- and `Ctrl+V` there called
/// `preventDefault` and went off to read the clipboard looking for a video, so
/// the first text field the app ever grew could not be pasted into. Found by
/// the owner trying to paste a word into Settings, one minute after it shipped.
///
/// Letting the keystroke through is also the more private answer: the webview
/// does the paste itself and Rust never touches the clipboard at all.
export function isTyping(event) {
  const el = event.target;
  if (!el) return false;
  return (
    el.isContentEditable ||
    el.tagName === "TEXTAREA" ||
    (el.tagName === "INPUT" && !["checkbox", "radio", "button"].includes(el.type))
  );
}

/// Says something on the screen where it belongs.
///
/// `screen` is where the person will be when the message matters -- usually the
/// screen that raised it, but a caller that navigates first names its
/// destination instead (setup's "Added N to the queue" moves you and then
/// speaks, so it names the queue).
///
/// # Two bugs, one line
///
/// This used to write to `#receipt` alone, which lives in the queue footer.
/// Review calls it too -- for "Saved", and for every error either of its
/// buttons can raise -- and while review is up the queue is `hidden`, so that
/// element renders 0x0. `Save & keep reviewing` did its work and said nothing,
/// and a failed `Send to chat` was indistinguishable from a button nobody had
/// wired. The app was reporting the whole time, into a box nobody could see.
///
/// The first fix was to write to every receipt at once, and that was worse in a
/// way only real use showed: a background paste landing while somebody was
/// reviewing put "Added 1 recording to the queue." directly beside the button
/// they had just pressed. A true sentence, in the one position that makes it
/// read as this button's answer. Broadcasting is not the same as reporting.
export function showReceipt(message, screen = "screen-queue") {
  const line = document.querySelector(`#${screen} [data-receipt]`);
  if (!line) return;
  line.textContent = message;
  clearTimeout(receiptTimer);
  receiptTimer = setTimeout(() => (line.textContent = ""), 8000);
}

// --- what the review screen is allowed to know -----------------------------

let reviewableListener = null;

/// Called after every reload with the handle the Review entry should open --
/// the newest waiting row, or failing that the newest reviewed one that still
/// has a scan -- and `{ waiting }` saying which kind it is, because the entry's
/// subtitle is a different sentence in each case. Null when there is neither.
export function markReviewable(listener) {
  reviewableListener = listener;
}

// --- the row menu ----------------------------------------------------------

const menu = document.getElementById("menu");
let menuHandle = null;

function openMenu(button, handle) {
  menuHandle = handle;
  const rect = button.getBoundingClientRect();
  menu.hidden = false;
  const width = menu.offsetWidth;
  menu.style.top = `${rect.bottom + 4}px`;
  menu.style.left = `${Math.max(8, rect.right - width)}px`;
}

function closeMenu() {
  menu.hidden = true;
  menuHandle = null;
}

document.getElementById("menu-remove").addEventListener("click", async () => {
  if (menuHandle) {
    try {
      await invoke("remove_recording", { handle: menuHandle });
    } catch (e) {
      showReceipt(String(e));
    }
  }
  closeMenu();
});

// --- wiring ----------------------------------------------------------------

document.getElementById("rows").addEventListener("click", async (event) => {
  // The fourth step, in the row rather than behind the ⋯. Approving unlocks a
  // recording; the frames still only travel when a model asks for them by
  // path, so somebody has to carry the path to the model. Hiding that behind a
  // menu made it a step nobody saw -- and a step nobody sees is a step nobody
  // takes.
  const copy = event.target.closest(".copy-prompt");
  if (copy) {
    event.stopPropagation();
    // Locked until approved. It says why in its own label rather than sitting
    // there dimmed and mute.
    if (copy.getAttribute("aria-disabled") === "true") {
      showReceipt(STR.copyLockedReason);
      return;
    }
    try {
      await invoke("copy_prompt", { handle: copy.dataset.handle });
      showReceipt(STR.promptCopied);
    } catch (e) {
      showReceipt(String(e));
    }
    return;
  }

  const dots = event.target.closest(".dots");
  if (dots) {
    openMenu(dots, dots.closest(".row").dataset.handle);
    event.stopPropagation();
    return;
  }
  // A row waiting on a person opens its review. Other rows have no screen
  // behind them yet, so they do nothing rather than pretending.
  const row = event.target.closest(".row.is-reviewable");
  if (row) location.hash = `review/${row.dataset.handle}`;
});
document.addEventListener("click", (event) => {
  if (!menu.hidden && !menu.contains(event.target)) closeMenu();
});

document.getElementById("tabs").addEventListener("click", (event) => {
  const tab = event.target.closest(".tab");
  if (tab) {
    state.tab = tab.dataset.tab;
    render();
  }
});

const searchBox = document.getElementById("search");
searchBox.addEventListener("input", () => {
  state.search = searchBox.value.trim().toLowerCase();
  render();
});

// Clear resets the view: search box and tab. It touches nothing on disk.
document.getElementById("clear").addEventListener("click", () => {
  searchBox.value = "";
  state.search = "";
  state.tab = "all";
  render();
});

const hideToggle = document.getElementById("hide-completed");
hideToggle.checked = state.hideCompleted;
hideToggle.addEventListener("change", () => {
  state.hideCompleted = hideToggle.checked;
  localStorage.setItem("hideCompleted", hideToggle.checked ? "1" : "0");
  render();
});

// --- filter ------------------------------------------------------------------

const filterBtn = document.getElementById("filter");
const filterMenu = document.getElementById("filter-menu");

function showFilter(open) {
  filterMenu.hidden = !open;
  filterBtn.setAttribute("aria-expanded", String(open));
  filterBtn.classList.toggle("is-active", open);
}

filterBtn.addEventListener("click", (e) => {
  e.stopPropagation();
  showFilter(filterMenu.hidden);
});

filterMenu.addEventListener("click", (e) => e.stopPropagation());
filterMenu.addEventListener("change", (e) => {
  state.found = e.target.value;
  // The button says so when it is doing something. A filter you cannot see is
  // a list that looks broken.
  filterBtn.classList.toggle("is-filtering", state.found !== "any");
  render();
});

// Anywhere else closes it, including Escape -- which is already handled below
// alongside the row menu.
document.addEventListener("click", () => showFilter(false));

document.addEventListener("keydown", (event) => {
  // Typing beats every shortcut on this screen: Ctrl+V into a field is a
  // paste into that field, and Ctrl+K is a `k`.
  if (isTyping(event)) return;
  if (event.ctrlKey && !event.shiftKey && event.key.toLowerCase() === "v") {
    // A person pressed paste in our window: the one in-window gesture. The
    // setup screen owns the same shortcut while it is up; answering here too
    // would read the clipboard twice for one keypress.
    if (location.hash === "#setup") return;
    event.preventDefault();
    invoke("paste_from_window").then(showReceipt, (e) => showReceipt(String(e)));
  }
  if (event.ctrlKey && event.key.toLowerCase() === "k") {
    event.preventDefault();
    searchBox.focus();
  }
  if (event.key === "Escape") {
    closeMenu();
    showFilter(false);
  }
});

listen("queue-changed", reloadSoon);
listen("paste-result", (event) => showReceipt(String(event.payload ?? "")));

load();
