/**
 * `output_mode` against a real socket -- S2.4.
 *
 * The thing worth testing here is not that three modes produce three shapes.
 * It is that the mode which hands over locations cannot be used to walk around
 * the review gate, and that the gate is still the first thing every mode meets.
 *
 * Same stand-in app as `no_core.test.ts`, and core is pointed at nothing for
 * the same reason: the ordinary path must not need it.
 */

import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import net from 'node:net';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

import { addressFile } from '../src/tray.js';

let home: string;
let originalHome: string | undefined;
let originalCore: string | undefined;
let servers: net.Server[] = [];

beforeEach(() => {
  originalHome = process.env.USERPROFILE ?? process.env.HOME;
  originalCore = process.env.FRAMEKEEP_CORE;
  home = fs.mkdtempSync(path.join(os.tmpdir(), 'framekeep-modes-'));
  process.env.USERPROFILE = home;
  process.env.HOME = home;
  process.env.FRAMEKEEP_CORE = path.join(home, 'no-such-framekeep-core.exe');
  fs.mkdirSync(path.join(home, '.framekeep'), { recursive: true });
});

afterEach(async () => {
  for (const server of servers) await new Promise((r) => server.close(r));
  servers = [];
  if (originalHome) {
    process.env.USERPROFILE = originalHome;
    process.env.HOME = originalHome;
  }
  if (originalCore === undefined) delete process.env.FRAMEKEEP_CORE;
  else process.env.FRAMEKEEP_CORE = originalCore;
  fs.rmSync(home, { recursive: true, force: true });
});

/**
 * An app holding one reviewed recording.
 *
 * `hidden` decides whether a person masked anything -- the one input that
 * changes which routes are open. The frame files are written for real because
 * the images route reads them.
 */
function appWith(suffix: string, opts: { hidden: boolean }): Promise<string> {
  const cache = path.join(home, 'cache', suffix);
  fs.mkdirSync(path.join(cache, 'redacted'), { recursive: true });
  fs.writeFileSync(path.join(cache, 'frame-00001.webp'), Buffer.alloc(64, 1));
  fs.writeFileSync(path.join(cache, 'frame-00002.webp'), Buffer.alloc(64, 2));
  fs.writeFileSync(path.join(cache, 'redacted', 'frame-00001.webp'), Buffer.alloc(64, 9));

  const first = opts.hidden
    ? { pts_time: 0, file: path.join(cache, 'redacted', 'frame-00001.webp'), redacted: true }
    : { pts_time: 0, file: path.join(cache, 'frame-00001.webp'), redacted: false };

  const reply = {
    handle: 'abcdef0123456789',
    source_path: 'C:/videos/demo.mp4',
    video: { width: 1624, height: 860, duration_ms: 24_127 },
    transcript: { status: 'absent' },
    frames: [first, { pts_time: 5, file: path.join(cache, 'frame-00002.webp'), redacted: false }],
    frame_count: 2,
    review: { reviewed_at: 1_780_000_100, found: opts.hidden ? 3 : 0, hidden: opts.hidden ? 1 : 0 },
  };

  return serve(suffix, (method) =>
    method === 'hello'
      ? { result: { capabilities: ['queue', 'ingest', 'frames', 'redaction'], version: '0.1.0', protocol: 1 } }
      : { result: reply },
  ).then(() => cache);
}

/** An app that has the recording and is still waiting for somebody to look at it. */
function appAwaiting(suffix: string): Promise<void> {
  return serve(suffix, (method) =>
    method === 'hello'
      ? { result: { capabilities: ['queue', 'frames'], version: '0.1.0', protocol: 1 } }
      : {
          error: {
            code: 'AWAITING_REVIEW',
            message: 'This recording is waiting for your review in Framekeep.',
          },
        },
  );
}

function serve(suffix: string, answer: (method: string) => object): Promise<void> {
  const address =
    process.platform === 'win32'
      ? `\\\\.\\pipe\\framekeep-v1-modes-${process.pid}-${suffix}`
      : path.join(home, `framekeep-v1-${suffix}.sock`);

  const server = net.createServer((socket) => {
    let buffer = '';
    socket.setEncoding('utf8');
    socket.on('data', (chunk: string) => {
      buffer += chunk;
      for (let i = buffer.indexOf('\n'); i >= 0; i = buffer.indexOf('\n')) {
        const line = buffer.slice(0, i);
        buffer = buffer.slice(i + 1);
        const request = JSON.parse(line) as { id: string; method: string };
        socket.write(`${JSON.stringify({ id: request.id, ...answer(request.method) })}\n`);
      }
    });
  });
  servers.push(server);
  return new Promise((resolve) =>
    server.listen(address, () => {
      fs.writeFileSync(addressFile(), address);
      resolve();
    }),
  );
}

interface Result {
  content: { type: string; text?: string }[];
  structuredContent?: Record<string, unknown>;
  isError?: boolean;
}

const textOf = (r: Result) => r.content.find((c) => c.type === 'text')?.text ?? '';
const images = (r: Result) => r.content.filter((c) => c.type === 'image');

/**
 * Both channels as one string, with every flavour of Windows separator flattened.
 *
 * The prose channel renders the details with `JSON.stringify`, so a path that
 * is `C:\cache\f.webp` in the structured channel is `C:\\cache\\f.webp` in the
 * text one. A test that searches for the first form finds nothing in the second
 * -- which reads exactly like "the path is not there", and would have passed
 * this file's whole point while the path sat in the reply. Measured the hard
 * way: the first version of these tests was wrong in both directions at once.
 */
const flat = (r: Result) =>
  (textOf(r) + JSON.stringify(r.structuredContent ?? {}))
    .replace(/\\\\/g, '/')
    .replace(/\\/g, '/');

const slash = (p: string) => p.replace(/\\/g, '/');

/** Every path the reply names, taken from the channel that holds them unescaped. */
const filesIn = (r: Result) =>
  ((r.structuredContent?.frames as { file?: string }[] | undefined) ?? [])
    .map((f) => f.file)
    .filter((f): f is string => typeof f === 'string');

describe('output_mode: files', () => {
  it('returns paths and no pictures when nobody hid anything', async () => {
    const cache = await appWith('clean', { hidden: false });
    const { videoFrames } = await import('../src/tools/video_frames.js');

    const result = (await videoFrames(
      { path: 'C:/videos/demo.mp4', output_mode: 'files' },
      undefined,
    )) as Result;

    expect(images(result)).toHaveLength(0);
    // The paths are really there -- which is what makes the "no paths"
    // assertions further down mean something. Checked in the structured
    // channel, where a path is still a path.
    expect(filesIn(result)).toContain(path.join(cache, 'frame-00001.webp'));
    // ...and really reach the prose channel too, escaping and all.
    expect(flat(result)).toContain(slash(path.join(cache, 'frame-00001.webp')));
    expect(result.structuredContent?.output_mode).toBe('files');
  });

  it('states the price of reading them, since the reply cap no longer does', async () => {
    await appWith('priced', { hidden: false });
    const { videoFrames } = await import('../src/tools/video_frames.js');

    const result = (await videoFrames(
      { path: 'C:/videos/demo.mp4', output_mode: 'files' },
      'claude-code',
    )) as Result;

    const perFrame = result.structuredContent?.tokens_per_frame as number;
    expect(perFrame).toBeGreaterThan(0);
    expect(result.structuredContent?.tokens_if_all_read).toBe(perFrame * 2);
  });

  it('is refused on a recording where a person hid something', async () => {
    // The reason this mode has a rule at all: a path is a step away from the
    // version the person hid things from.
    await appWith('masked', { hidden: true });
    const { videoFrames } = await import('../src/tools/video_frames.js');

    const result = (await videoFrames(
      { path: 'C:/videos/demo.mp4', output_mode: 'files' },
      undefined,
    )) as Result;

    expect(result.isError).toBe(true);
    expect(textOf(result)).toContain('not available');
    // Refused without handing over the map: no path, no directory name, in
    // either channel.
    expect(filesIn(result)).toHaveLength(0);
    expect(flat(result)).not.toContain('redacted/');
    expect(flat(result)).not.toContain('.webp');
    expect(flat(result)).not.toContain(slash(home));
  });

  it('does not get past the review gate', async () => {
    // The gate is the product. A new delivery mode must not be a new door.
    await appAwaiting('gate-files');
    const { videoFrames } = await import('../src/tools/video_frames.js');

    const result = (await videoFrames(
      { path: 'C:/videos/demo.mp4', output_mode: 'files' },
      undefined,
    )) as Result;

    expect(textOf(result)).toContain('waiting for your review');
    expect(textOf(result)).not.toContain('.webp');
    expect(images(result)).toHaveLength(0);
  });
});

describe('output_mode: text', () => {
  it('gives times and counts, and neither pictures nor paths', async () => {
    const cache = await appWith('listing', { hidden: false });
    const { videoFrames } = await import('../src/tools/video_frames.js');

    const result = (await videoFrames(
      { path: 'C:/videos/demo.mp4', output_mode: 'text' },
      undefined,
    )) as Result;

    expect(images(result)).toHaveLength(0);
    expect(textOf(result)).toContain('5.0s');
    expect(filesIn(result)).toHaveLength(0);
    expect(flat(result)).not.toContain(slash(cache));
    expect(flat(result)).not.toContain('.webp');
  });

  it('is available on a redacted recording, and says how many were masked', async () => {
    // Unlike `files`: a count of masked frames tells the model what happened
    // without telling it where anything is.
    await appWith('listing-masked', { hidden: true });
    const { videoFrames } = await import('../src/tools/video_frames.js');

    const result = (await videoFrames(
      { path: 'C:/videos/demo.mp4', output_mode: 'text' },
      undefined,
    )) as Result;

    expect(result.isError).toBeUndefined();
    expect(result.structuredContent?.redacted_frames).toBe(1);
    expect(textOf(result)).toContain('stay hidden');
  });

  it('does not get past the review gate either', async () => {
    await appAwaiting('gate-text');
    const { videoFrames } = await import('../src/tools/video_frames.js');

    const result = (await videoFrames(
      { path: 'C:/videos/demo.mp4', output_mode: 'text' },
      undefined,
    )) as Result;

    expect(textOf(result)).toContain('waiting for your review');
  });
});

describe('output_mode: images', () => {
  it('still sends the masked copy, and marks it masked', async () => {
    await appWith('imgs', { hidden: true });
    const { videoFrames } = await import('../src/tools/video_frames.js');

    const result = (await videoFrames(
      { path: 'C:/videos/demo.mp4', output_mode: 'images' },
      undefined,
    )) as Result;

    expect(images(result)).toHaveLength(2);
    const listed = result.structuredContent?.frames as { pts_time: number; redacted: boolean }[];
    // Stated, not left to be read off a directory name.
    expect(listed.find((f) => f.pts_time === 0)?.redacted).toBe(true);
    expect(listed.find((f) => f.pts_time === 5)?.redacted).toBe(false);
  });

  it('names no location, so the reply is not a route to the unmasked copy', async () => {
    // The reply already carries the pixels; the path buys the caller nothing
    // and sits one directory from what a person hid. `video_map` has always
    // listed frames without one -- this is the two tools agreeing at last.
    //
    // The positive control for these assertions is the `output_mode: files`
    // test above: same helpers, same probe, and there they DO find the path.
    // Without it, "no path in the reply" and "the probe is broken" read alike.
    const cache = await appWith('nopaths', { hidden: true });
    const { videoFrames } = await import('../src/tools/video_frames.js');

    const result = (await videoFrames({ path: 'C:/videos/demo.mp4' }, undefined)) as Result;

    expect(images(result)).toHaveLength(2);
    expect(filesIn(result)).toHaveLength(0);
    expect(flat(result)).not.toContain(slash(cache));
    expect(flat(result)).not.toContain('.webp');
    expect(flat(result)).not.toContain('redacted/');
  });
});

describe('a mode nobody offers', () => {
  it('is refused by name, before the app is even asked', async () => {
    // No server started: reaching the refusal proves nothing was consulted.
    const { videoFrames } = await import('../src/tools/video_frames.js');

    const result = (await videoFrames(
      { path: 'C:/videos/demo.mp4', output_mode: 'image' },
      undefined,
    )) as Result;

    expect(result.isError).toBe(true);
    expect(textOf(result)).toContain('output_mode must be one of');
  });
});
