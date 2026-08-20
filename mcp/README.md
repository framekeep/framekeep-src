# framekeep-mcp

Show your AI what you mean.

Framekeep turns a screen recording into something the AI you are already
talking to can actually see: key frames and a transcript, prepared on your own
machine. This package is the [Model Context
Protocol](https://modelcontextprotocol.io) adapter — the piece your AI client
talks to.

## What you need

This adapter is a shell. The work happens in the Framekeep app, which decodes
video, extracts frames, transcribes speech, and — the part that matters —
shows you every frame before any of it moves.

Install the app first. The adapter on its own has nothing to run.

You also need **Node.js 20 or newer**. The command below is `npx`, and your AI
client will start this server with `node` — without it the first step fails
with `'npx' is not recognized`, a message that says nothing about what is
actually missing.

## Setup

```
npx framekeep-mcp init
```

Run it **in the folder you work in**. `init` walks up to the nearest git
repository and writes there, falling back to the current folder when there is
no repository above it — either way it says which folder it chose and why. Run
it somewhere else and the config lands where nothing reads it, with no error,
because nothing went wrong. `--dir` overrides the choice.

| Client | File it writes |
|--------|----------------|
| Claude Code | `.mcp.json` |
| Cursor | `.cursor/mcp.json` |
| VS Code | `.vscode/mcp.json` |

Those are project files: you can see them in `git status`, and you remove
Framekeep by deleting them. Existing entries are merged, never overwritten.

**Codex** keeps its config in TOML and has no per-project file, so it is
skipped above and needs its own line:

```
npx framekeep-mcp init --global --client codex --yes
```

Anything outside the project is previewed first and waits for `--yes` — a
global config belongs to you, not to this tool.

Then **restart your client.** It will ask you to approve the Framekeep server
the first time, the same as any other MCP server.

### Why the command is typed once and never again

What `init` writes points at `node` and an absolute path, never at `npx`. That
is not a style choice: on Windows a client that spawns a bare `npx` fails with
`ENOENT`, and `npx.cmd` fails with `EINVAL`, because Node refuses to spawn
`.cmd` files without a shell. Typing the command yourself is the one place npx
works — your terminal has a shell — and the config it leaves behind keeps that
problem out of the daily path.

## What your AI can then do

| Tool | What it answers |
|------|-----------------|
| `video_map` | What is in this recording — how long, how many key frames, what was said, at what timestamps |
| `video_frames` | The frames themselves, for a moment or a range |

`video_map` first, `video_frames` second. The map is cheap and text-shaped;
the frames are images and cost real budget, so the model picks the moments it
needs rather than pulling everything.

## Nothing leaves unseen

Framekeep reads the text in each frame and flags what you probably do not want
to send — API keys, tokens, email addresses, card numbers, private keys, plus
any words you add yourself. You decide what is blacked out, and you can draw
over anything the scan missed.

Until you have approved a recording, this adapter will not serve its frames.
Asking again does not help, and that is deliberate: it tells your AI to go and
ask you.

**The scan is a helper, not a guarantee.** It reads text off pixels, and small
text is genuinely hard to read. The review screen is the part that does not
miss things, which is why nothing gets past it.

## After you approve, nothing is sent

Approving a recording **unlocks** it. It does not deliver it. There is no push
from Framekeep to your AI — no notification, no upload, no background channel.
Frames move in exactly one direction: your model asks for them, because you
asked your model something.

So the last step is yours:

```
Look at the screen recording at "C:\path\to\video.mp4" and tell me what happens in it.
```

The **path** is the part people miss. Your client does not receive the
recording when you approve it; it receives a question with a path in it, and
`video_map` does the rest. Most clients cannot be handed a video at all — a
path is what they can work with. The app has a **Copy prompt** button on each
approved row that puts exactly the line above on your clipboard, quoted,
because real paths have spaces in them.

## Where things live

Recordings, frames and transcripts stay on your machine, under `~/.framekeep`.
Nothing is sent to us — there is no server to send it to. The app makes one
kind of network request, and only when you ask: downloading a speech-to-text
model.

## Licence

Business Source License 1.1, converting to Apache 2.0 four years after each
version is released. You may use it for anything, including commercially and
inside a company. You may not sell it, or offer it as a service that competes
with Framekeep. See [LICENSE](LICENSE).
