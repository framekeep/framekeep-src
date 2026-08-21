// The Settings screen. Every switch here moves something.
//
// It did not always: controls were shown inert with a title explaining why,
// which was right while the screens were being built against a mockup and
// wrong for a build people install. Nobody hovers a toggle to read a tooltip
// before concluding the app is half-finished.
//
// Live: Theme (the data-theme hook) · Start on startup (Windows owns it; see
// startup.rs) · Minimize to tray (CloseRequested reads it live) · Always hide
// these words (S5.8) · Watch a folder (system picker in Rust — the webview
// never sees a path except to display it) · Auto-delete + keep-days (answering
// IS retention L5b's one-time question) · the danger zone. Dangerous buttons
// arm on first click and act on the second; three seconds un-arms them.
//
// Three switches were deliberately REMOVED rather than made to work, because
// their off position would have taken the person out of the loop: auto-detect,
// show-redacted-preview, and keep-frames. See BUILD-PROGRESS.
//
// Routing: "#settings" shows it, like every other screen.

import { showReceipt } from "./queue.js";

const { invoke } = window.__TAURI__.core;

const el = (id) => document.getElementById(id);

const STR = {
  confirm: "Click again to confirm",
  reset: "Settings are back to defaults.",
  watchOn: (folder) => `Watching ${folder} for new recordings.`,
  watchOff: "Folder watching is off.",
  watchSub: "Automatically import new recordings from a folder.",
  // Said on every add, because it is the part a person would otherwise get
  // wrong: nothing already reviewed is scanned again, so a word added today
  // does not reach back into yesterday's recordings.
  patternAdded: "Added. It applies to recordings you import from now on.",
  patternRemove: (word) => `Stop hiding ${word}`,
  startupSub: "Launch in background when your computer starts.",
  modelAuto: "Best for this machine",
  modelSub: "Choose how speech in your videos is transcribed.",
  // The size is said before the click, not after it -- half a gigabyte is a
  // decision, and a progress bar that starts first has made it for them.
  speechGetSub: (name, mib) =>
    `${name} — ${mib} MiB, downloaded from Hugging Face when you ask and kept on this machine.`,
  speechGetting: (percent) => `Downloading… ${percent}%`,
  // Says what changes and, just as importantly, what does not: recordings
  // already through the queue are not revisited, and a person who expects
  // yesterday's transcripts to appear would be waiting for nothing.
  speechGot: "Speech model installed. Recordings from now on will include a transcript.",
  // Said out loud because it is the number people are actually choosing
  // between: the default is slower than realtime, and `base` is fourteen
  // times faster. A dropdown of names hides that; a dropdown of names with
  // speeds does not.
  modelOption: (m) =>
    `${m.name} — ${m.size_mib} MiB, ${m.speed_measured ? "" : "~"}${Math.round(m.realtime_factor)}x` +
    (m.installed ? "" : " (not downloaded)"),
  modelsDirDefault: "~/.framekeep/models",

  // S5.9. The sections that report rather than switch.
  speechOn: "Ready",
  speechOff: "Unavailable",
  speechReady: (model) => `Whisper, running locally. Model: ${model}`,
  speechMissing: "Frames still work. Recordings arrive without a transcript.",
  cacheDetail: (folders, days) =>
    `${folders} recording${folders === 1 ? "" : "s"} cached, removed after ${days} days without use.`,
  // Raw numbers on purpose. This is the section for people who want the
  // number, and a friendlier phrasing would be an interpretation of a
  // measurement rather than the measurement.
  selection: (s) =>
    `Scene change above ${s.scene_threshold} · at most one frame every ${s.min_gap_seconds}s · ` +
    `at least one every ${s.max_gap_seconds}s.`,
  shortcutOn: "Active",
  shortcutOff: "Not available",
  diagUnknown: "Couldn't ask framekeep-core.",
  diagUnknownShort: "Unknown",
  unknownPath: "Not created yet",
};

let settings = null;

// --- render ------------------------------------------------------------------

function render() {
  el("st-theme").value = settings.theme;
  applyTheme(settings.theme);

  const watch = settings.watch;
  el("st-watch").checked = !!watch;
  el("st-watch-sub").textContent = watch ? STR.watchOn(watch.folder) : STR.watchSub;

  el("st-language").value = settings.transcription.language ?? "auto";
  el("st-models-dir").textContent =
    settings.transcription.models_dir ?? STR.modelsDirDefault;

  el("st-tray").checked = settings.app.minimize_to_tray;
  el("st-startup").checked = settings.app.start_on_login;

  el("st-autodelete").checked = settings.retention.delete_copied_sources;
  const days = String(settings.retention.keep_days);
  const keep = el("st-keepdays");
  keep.value = [...keep.options].some((o) => o.value === days) ? days : "7";
  keep.disabled = !settings.retention.delete_copied_sources;

  renderPatterns();
}

/// Built with DOM calls rather than a template string, and that is the whole
/// point: every one of these is text a person typed, and this screen is where
/// it comes back out. `textContent` cannot be talked into being markup.
function renderPatterns() {
  const list = el("st-patterns");
  list.replaceChildren();
  for (const word of settings.redaction.patterns) {
    const chip = document.createElement("span");
    chip.className = "pattern";
    const label = document.createElement("span");
    label.textContent = word;
    const remove = document.createElement("button");
    remove.type = "button";
    remove.textContent = "×";
    remove.setAttribute("aria-label", STR.patternRemove(word));
    remove.addEventListener("click", () => forget(word));
    chip.append(label, remove);
    list.append(chip);
  }
}

/// The CSS reads data-theme on the root; absent means "follow the OS".
function applyTheme(theme) {
  if (theme === "system") delete document.documentElement.dataset.theme;
  else document.documentElement.dataset.theme = theme;
}

// --- what this machine actually has ------------------------------------------
//
// Transcription, Privacy, Advanced and About describe the machine rather than
// the settings file, so they are filled from `diagnostics` -- core's own
// answer about the tools it found. Nothing here is written down twice: the
// ffmpeg version comes from ffmpeg, the selection numbers come from the code
// that selects, and a build that replaced either would be described correctly
// without anyone remembering to edit this file.
//
// Every field starts at `Checking…` in the markup and has to end somewhere
// else, the failure path included. A screen still saying `Checking…` a minute
// later is this app's oldest bug shape: work that stopped, reported as work
// still going.

/// `bundled · 8.1.2 · x64`, or the truth when it is less tidy than that.
function ffmpegLine(d) {
  const where = d.ffmpeg.from_path ? "from PATH" : "bundled";
  return [where, d.ffmpeg.version_short, d.arch].filter(Boolean).join(" · ");
}

function fillDiagnostics(d) {
  // -- Transcription --------------------------------------------------------
  const speechOn = d.whisper.available;
  const pill = el("st-speech-state");
  pill.textContent = speechOn ? STR.speechOn : STR.speechOff;
  pill.classList.toggle("is-on", speechOn);
  pill.classList.toggle("is-off", !speechOn);
  el("st-speech-detail").textContent = speechOn
    ? STR.speechReady(fileName(d.whisper.model))
    : STR.speechMissing;
  // Core already wrote this for a person to read, and it names the fix. It is
  // long, so it gets a row that wraps instead of one that clips it in half.
  el("st-speech-reason").hidden = speechOn;
  el("st-speech-reason-text").textContent = d.whisper.unavailable ?? "";

  // -- Privacy --------------------------------------------------------------
  el("st-cache-root").textContent = d.cache.root ?? STR.unknownPath;
  el("st-cache-detail").textContent = STR.cacheDetail(d.cache.folders, d.cache.keep_days);

  // -- Advanced -------------------------------------------------------------
  el("st-ffmpeg").textContent = ffmpegLine(d);
  el("st-ffmpeg-detail").textContent = d.ffmpeg.version ?? d.ffmpeg.path;
  el("st-ffmpeg-warning").hidden = !d.ffmpeg.from_path;
  el("st-selection").textContent = STR.selection(d.selection);

  // -- About ----------------------------------------------------------------
  el("st-core-version").textContent = `v${d.core_version}`;
  el("st-arch").textContent = d.arch;
}

/// What every diagnostics field says when core could not be asked. The message
/// itself goes to the receipt line; these keep the rows from lying by omission.
function diagnosticsUnavailable() {
  for (const id of [
    "st-speech-detail",
    "st-cache-detail",
    "st-cache-root",
    "st-ffmpeg",
    "st-ffmpeg-detail",
    "st-selection",
    "st-core-version",
    "st-arch",
  ]) {
    el(id).textContent = STR.diagUnknown;
  }
  const pill = el("st-speech-state");
  pill.textContent = STR.diagUnknownShort;
  pill.classList.remove("is-on", "is-off");
  el("st-speech-reason").hidden = true;
  el("st-ffmpeg-warning").hidden = true;
}

async function loadDiagnostics() {
  try {
    const d = await invoke("diagnostics");
    // ffmpeg missing is a report, not an error: everything else in it is still
    // true, and the reason is written for a person.
    if (!d.ffmpeg) {
      diagnosticsUnavailable();
      el("st-ffmpeg-detail").textContent = d.ffmpeg_error ?? STR.diagUnknown;
      el("st-core-version").textContent = `v${d.core_version}`;
      el("st-arch").textContent = d.arch;
      return;
    }
    fillDiagnostics(d);
  } catch (e) {
    diagnosticsUnavailable();
    say(String(e));
  }
}

/// Whether the global shortcut reached the OS. Asked once: registration
/// happens at startup and cannot change while the app is running, so a second
/// answer would only be the first one again.
let shortcutAsked = false;
async function loadShortcutStatus() {
  if (shortcutAsked) return;
  shortcutAsked = true;
  const pill = el("st-global-state");
  try {
    const s = await invoke("shortcut_status");
    pill.textContent = s.registered ? STR.shortcutOn : STR.shortcutOff;
    pill.classList.toggle("is-on", s.registered);
    pill.classList.toggle("is-off", !s.registered);
    if (!s.registered && s.detail) el("st-global-detail").textContent = s.detail;
  } catch (e) {
    // Asked and could not be answered is its own state, and not the same as
    // "not registered" -- claiming the shortcut is dead when we do not know
    // would send people looking for a problem they may not have.
    shortcutAsked = false;
    pill.textContent = STR.diagUnknownShort;
    pill.classList.remove("is-on", "is-off");
    say(String(e));
  }
}

function fileName(path) {
  return path ? path.split(/[\\/]/).pop() : "";
}

// --- routing -----------------------------------------------------------------

/// The six sections of Framekeepplan's Settings nav, in its order. `general`
/// lives at plain `#settings` so every link that already pointed here still
/// lands somewhere real.
const TABS = ["general", "transcription", "privacy", "shortcuts", "advanced", "about"];

/// Which section the address asks for, or `null` when the address is not
/// Settings at all. An unrecognised name falls back to General rather than
/// showing a screen with every pane hidden -- a blank main area reads as a
/// broken app, and a typo in a hash is not worth one.
function currentTab() {
  const m = location.hash.match(/^#settings(?:\/([a-z]+))?$/);
  if (!m) return null;
  return TABS.includes(m[1]) ? m[1] : "general";
}

function showTab(tab) {
  for (const name of TABS) {
    el(`pane-${name}`).hidden = name !== tab;
    const link = el(`tab-${name}`);
    link.classList.toggle("is-active", name === tab);
    if (name === tab) link.setAttribute("aria-current", "page");
    else link.removeAttribute("aria-current");
  }
  // Everything but General describes the machine rather than the settings
  // file, so it is asked for on the way in. Ask once per opening, not once per
  // process: a model that finished downloading between two visits should show
  // up on the second.
  if (tab === "transcription" || tab === "privacy" || tab === "advanced" || tab === "about") {
    loadDiagnostics();
  }
  if (tab === "shortcuts") loadShortcutStatus();
}

function route() {
  const tab = currentTab();
  const showing = tab !== null;
  el("screen-settings").hidden = !showing;
  if (showing) {
    showTab(tab);
    el("screen-queue").hidden = true;
    el("screen-review").hidden = true;
    for (const [id, on] of [["nav-queue", false], ["nav-review", false], ["nav-settings", true]]) {
      el(id).classList.toggle("is-active", on);
    }
    el("nav-settings").setAttribute("aria-current", "page");
  } else {
    el("nav-settings").classList.remove("is-active");
    el("nav-settings").removeAttribute("aria-current");
  }
}

el("nav-settings").addEventListener("click", () => (location.hash = "settings"));

// --- live controls -----------------------------------------------------------

function say(message) {
  const line = el("st-receipt");
  line.textContent = message;
  clearTimeout(say.timer);
  say.timer = setTimeout(() => (line.textContent = ""), 8000);
}

el("st-theme").addEventListener("change", async () => {
  const theme = el("st-theme").value;
  try {
    await invoke("set_theme", { theme });
    settings.theme = theme;
    applyTheme(theme);
  } catch (e) {
    say(String(e));
    render(); // the screen must show what is saved, not what was wished
  }
});

el("st-watch").addEventListener("change", async () => {
  const on = el("st-watch").checked;
  try {
    if (on) {
      const next = await invoke("watch_folder_on");
      if (next == null) {
        // They closed the picker: no folder, no watch, no change.
        render();
        return;
      }
      settings = next;
      say(STR.watchOn(next.watch.folder));
    } else {
      settings = await invoke("watch_folder_off");
      say(STR.watchOff);
    }
  } catch (e) {
    say(String(e));
  }
  render();
});

async function saveRetention() {
  try {
    await invoke("set_retention", {
      delete: el("st-autodelete").checked,
      keepDays: Number(el("st-keepdays").value),
    });
    settings.retention.delete_copied_sources = el("st-autodelete").checked;
    settings.retention.keep_days = Number(el("st-keepdays").value);
    settings.retention.choice_made = true;
  } catch (e) {
    say(String(e));
  }
  render();
}
// --- local model ------------------------------------------------------------

/// Filled from core's own catalogue, once, at boot.
///
/// If core cannot be reached the dropdown says so instead of standing empty:
/// an empty select is indistinguishable from a broken one, and this is the
/// screen where a person goes to find out why speech is not working.
async function loadModels() {
  const select = el("st-model");
  let report;
  try {
    report = await invoke("list_models");
  } catch (e) {
    select.replaceChildren(new Option("Can't reach framekeep-core", "auto"));
    select.disabled = true;
    el("st-model-sub").textContent = String(e);
    return;
  }

  const options = [new Option(STR.modelAuto, "auto")];
  for (const m of report.models) {
    options.push(new Option(STR.modelOption(m), m.name));
  }
  select.replaceChildren(...options);
  select.value = settings.transcription.model ?? "auto";
  // Short on purpose: this line is clipped to one line, and the sentence that
  // fit in a mockup lost its ending on the real window. What the dropdown
  // cannot say is *which* model "best" resolved to, so that is all this says.
  //
  // The model id is set in mono, per the type roles in tokens.md: sans is for
  // what we wrote, mono for what the machine produced. `large-v3-turbo-q5_0`
  // is a filename, and reading it in prose type invites reading it as prose.
  // Built with DOM calls, not innerHTML -- the id is a value, and values do
  // not get to be markup on this screen.
  const sub = el("st-model-sub");
  if (report.recommended && !settings.transcription.model) {
    const name = document.createElement("code");
    name.className = "mono";
    name.textContent = report.recommended;
    sub.replaceChildren("Currently ", name, ".");
  } else {
    sub.textContent = STR.modelSub;
  }

  offerDownload(report);
}

/// The download row, shown only when downloading would change anything.
///
/// Asked of the model list rather than of the doctor's sentence: doctor says
/// speech is unavailable for two different reasons -- no model, or no
/// whisper-cli -- and only one of them is fixed by fetching a file. Reading
/// which from its prose is the trap this codebase keeps naming; counting
/// installed models is not.
let pendingModel = null;

function offerDownload(report) {
  const row = el("st-speech-get");
  if (report.models.some((m) => m.installed)) {
    row.hidden = true;
    pendingModel = null;
    return;
  }
  // What core would pick for this machine, or the best-first head of the list
  // when it could not read the RAM.
  const want =
    report.models.find((m) => m.name === report.recommended) ?? report.models[0];
  if (!want) return;

  pendingModel = want;
  el("st-speech-get-sub").textContent = STR.speechGetSub(want.name, want.size_mib);
  row.hidden = false;
}

el("st-speech-download").addEventListener("click", async () => {
  if (!pendingModel) return;
  const button = el("st-speech-download");
  const total = pendingModel.size_mib * 1_048_576;
  button.disabled = true;

  // Progress is read off the part-file on disk, so the number cannot run ahead
  // of the bytes. It is a poll rather than a stream because there is nothing
  // to stream: core writes the file, and the file is the truth.
  const tick = setInterval(async () => {
    try {
      const done = await invoke("model_progress");
      if (done > 0) {
        button.textContent = STR.speechGetting(Math.min(99, Math.floor((done / total) * 100)));
      }
    } catch {
      /* a failed poll is not worth saying anything about */
    }
  }, 1000);

  try {
    const models = await invoke("download_model", { name: pendingModel.name });
    say(STR.speechGot);
    offerDownload(models);
    // The card above still says Unavailable until something asks again.
    await loadDiagnostics();
  } catch (e) {
    // core's own sentence: it names the checksum, the short read, or the
    // network, and it already tells the person what to do next.
    say(String(e));
  } finally {
    clearInterval(tick);
    button.disabled = false;
    button.textContent = "Download";
  }
});

async function saveTranscription() {
  try {
    settings = await invoke("set_transcription", {
      model: el("st-model").value,
      language: el("st-language").value,
    });
  } catch (e) {
    say(String(e));
  }
  render();
}
el("st-model").addEventListener("change", saveTranscription);
el("st-language").addEventListener("change", saveTranscription);

/// `Change` opens the folder picker. Always -- it used to be the path itself
/// that was the button, and a second click on an already-chosen path silently
/// reset it to the default. Two meanings on one target, the destructive one
/// invisible and unconfirmed: a person clicking a path expects a picker, not to
/// lose the folder they set. The way back to the default is picking it.
///
/// The path never comes from typing, same packaging rule as the watched folder.
el("st-models-change").addEventListener("click", async () => {
  try {
    const next = await invoke("pick_models_dir");
    if (next != null) settings = next;
  } catch (e) {
    say(String(e));
  }
  render();
});

// --- application ------------------------------------------------------------

el("st-tray").addEventListener("change", async () => {
  try {
    settings = await invoke("set_minimize_to_tray", { on: el("st-tray").checked });
  } catch (e) {
    say(String(e));
  }
  render();
});

/// Windows owns this one. The switch shows what the SYSTEM says afterwards,
/// not what was asked for: a person who disabled the task in Task Manager
/// cannot be overridden from here, and a switch that flipped anyway would be
/// lying until the next reboot proved it.
el("st-startup").addEventListener("change", async () => {
  const sub = el("st-startup-sub");
  try {
    settings = await invoke("set_start_on_login", { on: el("st-startup").checked });
    sub.textContent = STR.startupSub;
  } catch (e) {
    sub.textContent = String(e);
    say(String(e));
  }
  render();
});

el("st-autodelete").addEventListener("change", saveRetention);
el("st-keepdays").addEventListener("change", saveRetention);

// --- words to always hide (S5.8) ---------------------------------------------

/// Rust owns the verdict. This never decides a word is too short itself: one
/// copy of that rule already had to be mirrored into the app, and a third in
/// the webview is one more place for the three to disagree.
function note(message, bad) {
  const line = el("st-pattern-note");
  line.textContent = message;
  line.toggleAttribute("data-bad", !!bad);
}

async function remember() {
  const box = el("st-pattern");
  const word = box.value.trim();
  if (!word) return;
  try {
    settings = await invoke("add_pattern", { word });
    box.value = "";
    note(STR.patternAdded, false);
  } catch (e) {
    note(String(e), true);
  }
  render();
}

async function forget(word) {
  try {
    settings = await invoke("remove_pattern", { word });
    note("", false);
  } catch (e) {
    note(String(e), true);
  }
  render();
}

el("st-pattern-add").addEventListener("click", remember);
el("st-pattern").addEventListener("keydown", (e) => {
  // Enter submits. Nothing here is a form, so nothing does this for free, and
  // typing a word then pressing Enter is what everyone tries first.
  if (e.key === "Enter") {
    e.preventDefault();
    remember();
  }
});

// --- danger zone -------------------------------------------------------------

/// First click arms, second acts; three seconds stands down. No dialog to
/// misclick through, and the button itself says what a second click does.
function armed(button, act) {
  let timer = null;
  const label = button.textContent;
  button.addEventListener("click", async () => {
    if (button.dataset.armed) {
      clearTimeout(timer);
      delete button.dataset.armed;
      button.textContent = label;
      await act();
      return;
    }
    button.dataset.armed = "1";
    button.textContent = STR.confirm;
    timer = setTimeout(() => {
      delete button.dataset.armed;
      button.textContent = label;
    }, 3000);
  });
}

armed(el("st-clear"), async () => {
  try {
    const message = await invoke("clear_all_data");
    say(message);
    showReceipt(message);
  } catch (e) {
    say(String(e));
  }
});

armed(el("st-reset"), async () => {
  try {
    settings = await invoke("reset_settings");
    say(STR.reset);
    render();
  } catch (e) {
    say(String(e));
  }
});

// --- boot --------------------------------------------------------------------

window.addEventListener("hashchange", route);

settings = await invoke("get_settings");
render(); // applies the saved theme before anyone opens the screen
route();
// After the first paint: it spawns core, and the theme should not wait on it.
loadModels();

// The sidebar version and About's read the same answer, so there is one.
invoke("app_version")
  .then((v) => {
    el("app-version").textContent = `v${v}`;
    el("st-app-version").textContent = `v${v}`;
  })
  .catch(() => {}); // the sidebar keeps what the markup shipped with
