// The First Setup / Paste screen. The app's front door, shown on the first
// run and reachable from the nav ever after.
//
// What it may do is two Rust commands: paste_from_window (a person clicked the
// paste zone or pressed Ctrl+V — the same explicit-gesture rule as everywhere
// else) and ingest_dropped (the OS handed us paths a person dragged in). The
// clipboard itself stays unreachable from here, by design and by test.
//
// Routing: "#setup" shows it, anything else hides it. First run is decided by
// a localStorage flag — the same store the queue uses for its toggle — set the
// moment the person leaves the screen by any door: skip, continue, or a
// successful paste. Nothing nags twice.

import { showReceipt, isTyping } from "./queue.js";

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const el = (id) => document.getElementById(id);
const SEEN = "setupSeen";

const STR = {
  watching: (folder) => `Watching ${folder} for new recordings`,
  notWatching: "Automatically import new recordings from a folder.",
  // Says what to do with it, because the copy is not the point -- running it
  // in the right folder is, and that is the part a person can get wrong while
  // doing everything else right.
  commandCopied: (line) => `Copied ${line}. Run it in the folder you work in.`,
  // The same words the Settings screen uses for "let core decide".
  modelAuto: "Best for this machine",
};

// --- routing ---------------------------------------------------------------

function onSetup() {
  return location.hash === "#setup";
}

function route() {
  const showing = onSetup();
  el("screen-setup").hidden = !showing;
  // The shell (sidebar + queue/review) yields entirely; setup brings its own
  // intro panel, per the mockup's 390px layout.
  document.querySelector(".app").hidden = showing;
}

function leave() {
  localStorage.setItem(SEEN, "1");
  location.hash = "";
}

// --- the doors out ----------------------------------------------------------

el("su-skip").addEventListener("click", leave);
el("su-continue").addEventListener("click", leave);

// --- paste ------------------------------------------------------------------

let busy = false;

async function pasteHere() {
  if (busy) return;
  busy = true;
  try {
    const message = await invoke("paste_from_window");
    report(message);
  } catch (e) {
    report(String(e));
  } finally {
    busy = false;
  }
}

/// A queued paste moves on to the queue, where the new row is; an answer that
/// queued nothing stays here, spoken under the zone where the action happened.
function report(message) {
  if (message.startsWith("Added")) {
    leave();
    showReceipt(message);
  } else {
    const line = el("su-receipt");
    line.textContent = message;
    clearTimeout(report.timer);
    report.timer = setTimeout(() => (line.textContent = ""), 8000);
  }
}

el("su-paste").addEventListener("click", pasteHere);

// Ctrl+V while the setup screen is up. queue.js owns the same shortcut for the
// queue; checking the route keeps exactly one handler answering.
document.addEventListener("keydown", (event) => {
  if (isTyping(event)) return;
  if (onSetup() && event.ctrlKey && !event.shiftKey && event.key.toLowerCase() === "v") {
    event.preventDefault();
    pasteHere();
  }
});

// --- the rows that were only pictures ---------------------------------------

/// Watching a folder is the same command the Settings screen calls. One door,
/// so the two screens cannot end up with different ideas of what is watched --
/// and the picker is the whole reason this is not `broadFileSystemAccess`.
el("su-watch").addEventListener("change", async () => {
  const box = el("su-watch");
  const sub = box.closest(".setup-row").querySelector(".row-text p");
  try {
    if (box.checked) {
      const next = await invoke("watch_folder_on");
      if (next == null) {
        box.checked = false; // they closed the picker: no folder, no watch
        return;
      }
      sub.textContent = STR.watching(next.watch.folder);
    } else {
      await invoke("watch_folder_off");
      sub.textContent = STR.notWatching;
    }
  } catch (e) {
    box.checked = !box.checked;
    // Spoken here, not in the queue footer. The person is standing on this
    // screen with a toggle that just sprang back, and until now the reason went
    // to a line on a screen they were not looking at -- the same silence that
    // made two review buttons look dead.
    report(String(e));
  }
});

/// These two rows describe settings rather than owning them. Sending someone
/// to the one screen that does own them beats a second copy of the controls.
for (const id of ["su-model", "su-advanced"]) {
  el(id).addEventListener("click", () => {
    leave();
    location.hash = "settings";
  });
}

// --- drop -------------------------------------------------------------------
//
// Tauri delivers real OS paths for drags onto the window. The decision about
// what those paths mean lives in Rust (paste::decide_files) — the same table a
// paste goes through, so drop and paste can never disagree about a format.

listen("tauri://drag-enter", () => {
  if (onSetup()) el("su-paste").classList.add("is-drop");
});
listen("tauri://drag-leave", () => el("su-paste").classList.remove("is-drop"));
listen("tauri://drag-drop", async (event) => {
  el("su-paste").classList.remove("is-drop");
  if (!onSetup()) return;
  const paths = event.payload?.paths ?? [];
  if (!paths.length) return;
  try {
    report(await invoke("ingest_dropped", { paths }));
  } catch (e) {
    report(String(e));
  }
});

// The model row showed `Whisper (Local)` no matter what was chosen -- true of
// every option, so it distinguished nothing and never changed. It says what is
// actually selected now, in the same words the Settings screen uses.
invoke("get_settings")
  .then((s) => {
    el("su-model").firstChild.textContent = s.transcription.model ?? STR.modelAuto;
  })
  .catch(() => {});

// --- connect your AI --------------------------------------------------------

// The command is rendered from Rust rather than trusted from the markup, so
// the line on screen and the line the button copies cannot drift apart. The
// markup ships the same string as a fallback: if this call ever fails the row
// still says something true rather than sitting blank.
invoke("connect_command")
  .then((line) => (el("su-init-cmd").textContent = line))
  .catch(() => {});

el("su-copy-init").addEventListener("click", async () => {
  try {
    report(STR.commandCopied(await invoke("copy_connect_command")));
  } catch (e) {
    report(String(e));
  }
});

// --- wiring -----------------------------------------------------------------

// The nav entry is a real door now.
const nav = el("nav-setup");
nav.removeAttribute("aria-disabled");
nav.addEventListener("click", () => (location.hash = "setup"));

window.addEventListener("hashchange", route);

// First run: no flag, no deep link → the front door.
if (!localStorage.getItem(SEEN) && !location.hash) {
  location.hash = "setup";
}
route();
