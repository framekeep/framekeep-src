/**
 * The bridge: what a person decided, and which files this reply may carry.
 *
 * The property under test is one sentence -- a recording the app has and a
 * person approved is served from its redacted copies -- plus the two states
 * that must never be confused with it: waiting on a person, and never seen by
 * the app at all.
 *
 * Everything here runs against a stand-in app on a real socket, so the answers
 * travel the same JSON Lines the shipping one speaks.
 */

import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import net from 'node:net';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

import { addressFile } from '../src/tray.js';
import { reviewedFrames } from '../src/reviewed.js';

let home: string;
let originalHome: string | undefined;
let servers: net.Server[] = [];

beforeEach(() => {
  originalHome = process.env.USERPROFILE ?? process.env.HOME;
  home = fs.mkdtempSync(path.join(os.tmpdir(), 'framekeep-bridge-test-'));
  process.env.USERPROFILE = home;
  process.env.HOME = home;
  fs.mkdirSync(path.join(home, '.framekeep'), { recursive: true });
});

afterEach(async () => {
  for (const server of servers) await new Promise((r) => server.close(r));
  servers = [];
  if (originalHome) {
    process.env.USERPROFILE = originalHome;
    process.env.HOME = originalHome;
  }
  fs.rmSync(home, { recursive: true, force: true });
});

function testAddress(suffix: string): string {
  return process.platform === 'win32'
    ? `\\\\.\\pipe\\framekeep-v1-bridge-${process.pid}-${suffix}`
    : path.join(home, `framekeep-v1-${suffix}.sock`);
}

/** An app that answers `hello` with the given capabilities, then one method. */
function fakeApp(
  suffix: string,
  capabilities: string[],
  answer: (method: string) => unknown,
): Promise<void> {
  const address = testAddress(suffix);
  const server = net.createServer((socket) => {
    let buffer = '';
    socket.setEncoding('utf8');
    socket.on('data', (chunk: string) => {
      buffer += chunk;
      for (let i = buffer.indexOf('\n'); i >= 0; i = buffer.indexOf('\n')) {
        const line = buffer.slice(0, i);
        buffer = buffer.slice(i + 1);
        const request = JSON.parse(line) as { id: string; method: string };
        const body =
          request.method === 'hello'
            ? { result: { capabilities, version: '0.1.0', protocol: 1 } }
            : (answer(request.method) as object);
        socket.write(`${JSON.stringify({ id: request.id, ...body })}\n`);
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

describe('what the app says about a recording', () => {
  it('hands back the redacted copies for a reviewed recording', async () => {
    await fakeApp('reviewed', ['queue', 'ingest', 'frames', 'redaction'], () => ({
      result: {
        frames: [
          { pts_time: 0, file: 'C:/cache/h/redacted/frame-00001.webp', redacted: true },
          { pts_time: 5, file: 'C:/cache/h/frame-00002.webp', redacted: false },
        ],
        review: { reviewed_at: 100, found: 3, hidden: 1 },
      },
    }));

    const state = await reviewedFrames('C:/videos/demo.mp4');
    expect(state.kind).toBe('reviewed');
    if (state.kind !== 'reviewed') return;
    expect(state.frames[0].file).toContain('redacted');
    expect(state.frames[0].redacted).toBe(true);
    // The frame nobody masked comes back untouched, and says so -- "hidden"
    // and "nothing needed hiding" must not look the same to a caller.
    expect(state.frames[1].redacted).toBe(false);
    expect(state.hiddenFrames).toBe(1);
    expect(state.found).toBe(3);
  });

  it('refuses, with the app’s own sentence, while a person has not looked', async () => {
    await fakeApp('awaiting', ['queue', 'frames'], () => ({
      error: {
        code: 'AWAITING_REVIEW',
        message: 'This recording is waiting for your review in Framekeep -- 2 sensitive items.',
      },
    }));

    const state = await reviewedFrames('C:/videos/demo.mp4');
    expect(state.kind).toBe('awaiting');
    if (state.kind !== 'awaiting') return;
    // The app's wording travels through unchanged: it is the one that knows
    // how many items and where to go.
    expect(state.message).toContain('waiting for your review');
  });

  it('falls back to reading the file when the app never saw it', async () => {
    await fakeApp('notfound', ['queue', 'frames'], () => ({
      error: { code: 'NOT_FOUND', message: 'No recording called `abc` in Framekeep.' },
    }));

    const state = await reviewedFrames('C:/videos/never-pasted.mp4');
    expect(state).toEqual({ kind: 'absent', appPresent: true });
  });

  it('does not ask an older app for something it never claimed', async () => {
    // The rule this protocol was built on: read the capability list, never
    // infer from a version. An app without `frames` is asked nothing.
    let asked: string[] = [];
    await fakeApp('older', ['queue', 'ingest'], (method) => {
      asked.push(method);
      return { error: { code: 'NOT_READY', message: 'not built' } };
    });

    const state = await reviewedFrames('C:/videos/demo.mp4');
    expect(state).toEqual({ kind: 'absent', appPresent: true });
    expect(asked).not.toContain('video.map');
  });

  it('says the app is absent when nothing is listening', async () => {
    const state = await reviewedFrames('C:/videos/demo.mp4');
    expect(state).toEqual({ kind: 'absent', appPresent: false });
  });

  it('treats a broken answer as absent rather than guessing', async () => {
    await fakeApp('broken', ['queue', 'frames'], () => ({
      error: { code: 'CORE_FAILED', message: 'the queue would not open' },
    }));

    const state = await reviewedFrames('C:/videos/demo.mp4');
    // Not a crash and not a silent success: the reply falls back to the path
    // that warns review was skipped, which is the truthful one.
    expect(state).toEqual({ kind: 'absent', appPresent: true });
  });
});
