/**
 * Talking to `framekeep-core`.
 *
 * This file is the whole reason the MCP adapter stays a shell: every question
 * about video goes through here, as an argument array, to a binary that knows
 * the answers. No ffmpeg command ever appears in this package.
 */

import { execFile, spawn } from 'node:child_process';
import { existsSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';

const run = promisify(execFile);
const here = dirname(fileURLToPath(import.meta.url));

export class CoreNotFound extends Error {
  constructor(public readonly lookedIn: string[]) {
    super(
      "Couldn't find framekeep-core.\n" +
        'Set FRAMEKEEP_CORE to its full path, or reinstall the app.\n' +
        `Looked in:\n  ${lookedIn.join('\n  ')}`,
    );
  }
}

export class CoreFailed extends Error {
  constructor(
    public readonly command: string,
    public readonly stderr: string,
  ) {
    // core writes human-facing errors to stderr and keeps stdout machine-clean,
    // so its own message is the best one to pass on -- it already says what
    // broke and what to do next.
    super(stderr.trim() || `framekeep-core ${command} failed without saying why.`);
  }
}

let cached: string | undefined;

/**
 * Finds the binary the same way the product ships it, rather than assuming a
 * location. `FRAMEKEEP_CORE` wins so a developer can point at a debug build
 * without reinstalling anything.
 */
export function locateCore(): string {
  if (cached) return cached;

  const name = process.platform === 'win32' ? 'framekeep-core.exe' : 'framekeep-core';
  const candidates = [
    process.env.FRAMEKEEP_CORE,
    join(here, '..', 'bin', name),
    // Dev layout: this package sitting beside core/ in the repo.
    join(here, '..', '..', 'core', 'target', 'release', name),
    join(here, '..', '..', '..', 'core', 'target', 'release', name),
  ].filter((p): p is string => Boolean(p));

  const found = candidates.find((p) => existsSync(p));
  if (!found) throw new CoreNotFound(candidates);
  cached = found;
  return found;
}

/**
 * Runs core and parses its stdout as JSON.
 *
 * Arguments go as an array, never as a concatenated string. The mandatory test
 * case is `C:\\Users\\Nguyễn Văn A\\Videos\\test.mp4`, and both competitors
 * audited in S0.4 break on exactly this.
 */
async function json<T>(args: string[]): Promise<T> {
  const bin = locateCore();
  try {
    // Frames are megabytes of JSON path lists at worst; the default 1 MB buffer
    // is not enough for a long recording.
    const { stdout } = await run(bin, args, { maxBuffer: 64 * 1024 * 1024 });
    return JSON.parse(stdout) as T;
  } catch (e) {
    const err = e as { stderr?: string; message?: string };
    throw new CoreFailed(args[0] ?? '?', err.stderr ?? err.message ?? '');
  }
}

export interface Probe {
  duration_seconds: number | null;
  width: number | null;
  height: number | null;
  codec: string | null;
  fps: number | null;
  has_audio: boolean;
  is_hdr: boolean;
}

export interface FrameEntry {
  pts_time: number;
  file: string;
}

export interface Segment {
  start_seconds: number;
  end_seconds: number;
  text: string;
}

export interface VideoMap {
  path: string;
  handle: string;
  video: Probe;
  selection: {
    threshold: number;
    min_gap_seconds: number;
    max_gap_seconds: number;
    selected: number;
    kept: number;
    dropped: number;
  };
  frames: FrameEntry[];
  transcript: { has_audio: boolean; model: string | null; segments: Segment[] } | null;
  transcript_from_cache: boolean;
  timing: { frames_seconds: number; transcript_seconds: number | null; total_seconds: number };
}

export type TranscriptStatus =
  | { status: 'absent' }
  | { status: 'running'; since_unix: number; stale: boolean }
  | { status: 'ready'; has_audio: boolean; model: string | null; created_unix: number; segments: Segment[] }
  | { status: 'failed'; error: string; at_unix: number };

export function probe(path: string): Promise<Probe> {
  return json<Probe>(['probe', path]);
}

/**
 * The map without the wait.
 *
 * `--skip-transcript` is the whole point: frames are ready in about 2.3 seconds
 * for SDR, while speech takes ~110 for the same recording. Blocking on the slow
 * half would throw away the only real lever this design has.
 *
 * WebP by default, and core does the encoding rather than this package: video
 * processing lives in `core/`, and an ffmpeg call in `mcp/` would be in the
 * wrong place (`AGENTS.md`). Lossless, so nothing is traded away -- 3.9x fewer
 * bytes for the same pixels. It buys disk and I/O, not reply room: the cap
 * counts pixel area, so PNG and WebP fit the same nine frames.
 */
export function mapFramesOnly(path: string, format: 'png' | 'webp' = 'webp'): Promise<VideoMap> {
  return json<VideoMap>(['map', path, '--skip-transcript', '--format', format]);
}

export interface Region {
  x1: number;
  y1: number;
  x2: number;
  y2: number;
}

export interface CropResult {
  path: string;
  output_dir: string;
  region: Region & { width: number; height: number };
  area_kept_percent: number;
  frames: { pts_time: number; file: string }[];
}

/**
 * Crops the frames already on disk to a rectangle, at their original size.
 *
 * This is the budget lever, not a convenience. A client's reply cap counts
 * pixel area, so a crop is the only way to fit more frames without spending a
 * pixel of what is kept -- scaling would buy the same room by blurring the text
 * the recording exists to show.
 */
export function crop(path: string, region: Region): Promise<CropResult> {
  const spec = `${region.x1},${region.y1},${region.x2},${region.y2}`;
  return json<CropResult>(['crop', path, '--region', spec, '--format', 'webp']);
}

export function transcriptStatus(path: string): Promise<TranscriptStatus> {
  return json<TranscriptStatus>(['transcript', path]);
}

/**
 * Starts transcription in a process that outlives this one, and does not wait.
 *
 * # `unref()` alone was not enough, and the symptom was invisible
 *
 * The first version called `execFile` and `unref`ed the child. On Windows that
 * still leaves it inside the parent's process tree, so every time the MCP client
 * exited it took the transcriber with it. Three separate runs reported
 * `transcript: running` and never finished -- there was no error, no warning,
 * and the lease looked healthy for ten minutes before going stale. Speech simply
 * never arrived.
 *
 * `detached: true` puts the child in its own process group, and `stdio: 'ignore'`
 * releases the pipes that would otherwise keep a handle open between them. Both
 * are needed; `unref` only stops us waiting.
 *
 * core takes a lease, so starting this twice is harmless -- the second process
 * is refused rather than duplicating the work.
 */
export function startTranscriptionInBackground(path: string): void {
  try {
    const child = spawn(locateCore(), ['transcribe', path], {
      detached: true,
      stdio: 'ignore',
      windowsHide: true,
    });
    // Nothing to handle, but the listener has to exist: an unhandled 'error' on
    // a ChildProcess throws and would take down the server for a failure that
    // is already recorded in the transcript store.
    child.on('error', () => {});
    child.unref();
  } catch {
    // Nothing to report to the caller: the map is already useful without words,
    // and the transcript status says so rather than lying about it.
  }
}
