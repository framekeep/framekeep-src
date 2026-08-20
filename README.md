# Framekeep

**Show your AI what you mean.**

Framekeep is a local MCP server that turns a screen recording into something the model
you are already chatting with can actually see: real frames as image blocks, plus a
timestamped transcript.

It does not analyse the video. It does not interpret it, summarise it, or draw
conclusions from it — the model you are talking to does all of that, and it is better at
it than a tool with no idea what you were trying to do.

> **Framekeep is the eye, not the brain.**

Processing happens on your device. Your video never leaves your machine; the frames you
approve are what travel into the conversation.

---

## How it works

1. Paste or drop a screen recording into the Framekeep app.
2. It extracts the frames that actually differ, transcribes the audio locally, and scans
   the frames for things that should not leave — API keys, tokens, emails.
3. **You review it.** Anything found is shown to you with a box around it, and you decide
   what gets hidden. Nothing reaches a model until you say so.
4. **You ask your AI about it.** It calls `video_map` to see what is in the recording,
   then `video_frames` for the moments it cares about.

There is no send-anyway button, and there is not going to be one. A recording that has
not been reviewed answers `AWAITING_REVIEW` and nothing else.

### Approving unlocks. It does not send.

Step 4 is a step, not a formality, and it is the one the app used to leave unsaid.
Approving a recording changes what Framekeep is *willing* to serve; it moves nothing.
There is no push from the app to your client — no notification, no upload, no background
channel — and frames travel only when a model asks for them.

What your client needs from you is the **path**. It never receives the recording; it
receives a question containing a path, and takes it from there:

```
Look at the screen recording at "C:\path\to\video.mp4" and tell me what happens in it.
```

Most clients cannot be handed a video file at all, which is why the app puts that exact
line — quoted, because real paths have spaces — on your clipboard from the **Copy
prompt** button on any approved row.

### On redaction, precisely

Automatic detection finds API keys, tokens and emails, and it **never sends a frame you
have not seen**. It is not a guarantee that everything sensitive was found — measured on
screen text, detection falls off sharply as type gets smaller. The review step is the
part that holds, which is why it is not optional.

---

## Install

The desktop app is Windows-first and not on the Microsoft Store yet.

The MCP adapter is on npm and works on its own — without the app it reads a video
directly and says plainly that nothing was reviewed:

```bash
npx framekeep-mcp init
```

Run it **in the folder you work in**: `init` walks up to the nearest repository and
writes there, falling back to the current folder, and says which it chose. From anywhere
else the config lands where no client reads it, and nothing reports a problem because
nothing went wrong.

That writes the server into the config of Claude Code, Cursor and VS Code, merging into
whatever is already there rather than overwriting it. Codex keeps its config in TOML and
has no per-project file, so it needs:

```bash
npx framekeep-mcp init --global --client codex --yes
```

Restart the client and ask it something:

```
Call video_map on C:\path\to\video.mp4 and tell me what is in it.
```

Runs against four clients: Claude Code · Cursor · VS Code · Codex.

---

## The two tools

**`video_map`** — what is in the recording and when. Cheap, no images, no side effects.
Call it first. Calling it again is also how you ask whether the transcript has finished.

**`video_frames`** — the frames themselves, for a time range you name. About nine
full-screen frames fit in one reply; `region` crops to part of the screen at full
resolution, which is the honest way to fit more. `output_mode` chooses how they arrive:
`images` (default), `files` (paths you open yourself, refused where anything was hidden),
or `text` (what exists and when, no pictures).

There is no third tool for the transcript, deliberately. Every extra tool is one more
thing a model has to learn to choose correctly.

---

## What is where

```
core/    Rust. The only part that knows how to handle video. No UI, no server.
         probe · select · dedup · encode · map · transcribe · OCR · redact
tray/    Rust. The app, the queue, and the IPC server. Owns the review gate.
mcp/     TypeScript. The MCP adapter — a shell that knows the protocol,
         the reply budget, and which channel actually reaches the model.
```

Each has its own README covering how to run it and why it is built that way.

## Building from source

`core` needs ffmpeg, which is not committed. Fetch the same build it is tested against —
LGPL, not GPL, and the *shared* variant:

```bash
gh release download latest --repo BtbN/FFmpeg-Builds   --pattern "ffmpeg-n8.1-latest-win64-lgpl-shared-8.1.zip"
```

`ffmpeg-n8.1-latest-win64-lgpl-shared-8.1.zip` · 67.6 MB · SHA-256
`4db007607ea8c9e18ed40bd0c000c93fe16c7774def523e93533ce1846d6c320` · reports version
`n8.1.2-40-g852b0552f0-20260814`. Unzip it and copy the contents of `bin/` into
`vendor/ffmpeg/win64/`, then run `framekeep-core doctor`.

The build carries no `--enable-gpl` and disables libx264 and libx265; `--enable-version3`
makes it LGPL v3. Framekeep only decodes, so the GPL encoders are not needed.

The three gates, one per crate:

```bash
cd core && cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
cd tray/src-tauri && cargo fmt --check && cargo clippy --features gui --all-targets -- -D warnings && cargo test
cd mcp && npm run build && npx vitest run
```

## License

Framekeep is **source-available, not open source.** [BUSL-1.1](LICENSE) covers everything
in this repository that does not carry a licence of its own; the adapter under `mcp/` keeps
[its own copy](mcp/LICENSE) because it ships to npm as a package in its own right, and the
terms are identical.

The Additional Use Grant covers using, running and modifying it for any purpose, including
inside a company and as part of paid work. What the licence withholds is selling Framekeep
or offering it as a service that competes with it — which is also why calling it *open
source* would be wrong: that restriction is on the field of use, and an OSI-approved
licence may not have one.

Each version converts to **Apache 2.0 four years** after it is first made publicly
available.

---

## Where the rest lives

This repository is the source. The product itself, the privacy policy, the
security page and the changelog live at [framekeep.app](https://framekeep.app).

Framekeep is **source-available**, not open source: the licence below lets you
use, run and modify it for anything, including commercially, and forbids only
reselling it or offering it as a competing service. Each version becomes
Apache 2.0 four years after it is released.
