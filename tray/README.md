# framekeep-tray

The app: the window, the queue, and the IPC server that the MCP adapter talks
to. It knows nothing about video — `framekeep-core` does all of that.

```
framekeep-core    all video processing. No UI, no IPC server.
framekeep-tray    this. GUI + queue + IPC server. Calls core for the work.
framekeep-mcp     MCP adapter. Tries IPC first; falls back to core alone.
```

## What is built

| | |
|---|---|
| **S3.2** IPC server | ✅ named pipe (Windows) · Unix socket (macOS/Linux) |
| **S3.3** `hello` handshake | ✅ protocol check, capabilities, `PROTOCOL_MISMATCH` |
| **S3.4** queue and retention | ✅ `~/.framekeep/queue.db`, expiry, orphan sweep |
| **S3.5** queue methods | ✅ `queue.list` · `queue.get` · `video.status`, plus the review gate |
| **S3.6** permission boundary | ✅ `framekeep-mcp` is refused `video.ingest` and `redaction.apply` |
| **S3.7** found by the adapter | ✅ the address is published to `~/.framekeep/ipc-address` at startup |
| **S3.1** tray icon and window | ✅ Tauri 2, behind the `gui` feature |
| `video.map` / `video.frames` | ⬜ they need the bridge to `framekeep-core`; they answer `NOT_READY` today, on purpose |
| Real screens | ⬜ S4/S5. Today's window says only what is true, and nothing else |

The address file exists because Node cannot read this account's SID, and so
cannot work out `\\.\pipe\framekeep-v1-<SID>` for itself. It is a hint, not a
credential: anything running as this user could rewrite it, and could equally
run `framekeep-core` directly. Its absence is the useful part — no file means no
app, which the adapter learns without spending any of its 300 ms budget.

Tauri sits behind the `gui` feature, and that is deliberate: `cargo test` and
the Linux CI job need no webview toolchain, so the protocol stays testable on a
machine with no display while CI proves the boundary on every push.

## Run it

The app:

```
cargo run --manifest-path tray/src-tauri/Cargo.toml --features gui --bin framekeep-tray
```

Tray icon in the corner, window on screen. Closing the window hides it — the
app's whole job is to wait in the tray — left-click the icon brings it back,
right-click is Open/Quit. Launching a second copy focuses the first instead of
erroring, and the order that makes that true is load-bearing: the
single-instance plugin registers before the pipe is bound. The first version
did it the other way round, and a second launch died at an error box.

Headless, for CI and protocol work:

```
cargo run --manifest-path tray/src-tauri/Cargo.toml --bin framekeep-trayd
```

It prints the address it bound. `--print-address` prints it and exits;
`--address <name>` binds somewhere else, which is how the tests keep out of each
other's way. Both binaries share one startup path (`bring_up` in `lib.rs`), so
neither can drift into a sequence the other forgot.

To watch a client's side of the same conversation:

```
node spike/s3-node-pipe-client/client.mjs
```

That probe connects the way `framekeep-mcp` will — a separate Node process, over
the real pipe with the real access rule — and prints every answer, including the
two refusals.

## The address

| Platform | Address |
|---|---|
| Windows | `\\.\pipe\framekeep-v1-<your SID>` |
| macOS / Linux | `~/.framekeep/run/framekeep-v1.sock` |

Not a localhost TCP port. Any process on the machine can reach a TCP port,
including a web page through a browser; both of these carry an OS-level access
rule instead. The `v1` is the protocol version, so an incompatible future
protocol listens elsewhere rather than meeting an old peer halfway through a
conversation.

Windows needs four things here, and all four are load-bearing: the user's SID in
the name (the pipe namespace is machine-wide), an explicit DACL for that SID
alone, `FILE_FLAG_FIRST_PIPE_INSTANCE` so nothing can squat the name, and
`PIPE_REJECT_REMOTE_CLIENTS` so it is not reachable over SMB.

## The queue

`~/.framekeep/queue.db` holds handles, file names, stages and counts. It holds no
transcript, no OCR text and no detected secret values — those live once each
under `~/.framekeep/cache/<handle>/`, so deleting a row deletes everything the
row knew about.

Entries expire seven days after they arrive, and the clock does not slide when a
recording is used again. Framekeep deletes a source video only when Framekeep
wrote it, only while it is still in `~/Framekeep/Recordings`, and only after the
user has answered the one-time question about it. Everything else is left alone
with a recorded reason.

The rules and the reasoning are in [`docs/spec-s3-retention.md`](../docs/spec-s3-retention.md);
`src/retention.rs` decides and `src/queue.rs` acts, and they are separate so the
first can be tested exhaustively without the second.

Two tests guard the shape rather than the behaviour: one pins the column list,
one pins the table list. A full-text index over transcripts would be the
competitor's permanent memory built by our own hand, and it would arrive looking
like a reasonable feature request.

## The boundary

`framekeep-mcp` can never call `video.ingest` or `redaction.apply`. The model
does not add recordings and does not approve its own redaction — only a person
at the window does.

It is enforced in `src/method.rs` as one exhaustive match, so a new method does
not compile until someone says who may call it. Six tests go red if the rule is
widened, one of them over a real pipe.

This stops the *model*. It is not a defence against other software running as
the same user: that software could claim to be the tray, or skip this process
and run `framekeep-core` itself.

## Testing

```
cargo test --manifest-path tray/src-tauri/Cargo.toml
```

53 tests. Most never open a pipe — `Session::handle_line` takes bytes and
returns a reply, so the handshake, the version mismatch, the boundary and every
malformed input are checked without any transport at all. The ones that do open
a pipe cover what only a real pipe can show: a second instance being refused the
name, and two clients being served at once.

Two of them are worth knowing about because they are shaped to fail. One scans
the raw bytes of `queue.db` for a sentence that was only ever written to a
transcript file, and asserts it is absent — then asserts the scan can find
something that *is* there, so its silence means something. The other runs a
recording through its whole lifecycle before checking that the database still
has exactly one table.

The Unix socket transport is written and its tests run on Linux in CI. **It has
not run on macOS** — there is no Mac in this project yet. That is S7.
