/**
 * The loop has to close for someone who did not build this repo.
 *
 * They install the app from the Store and run `npx framekeep-mcp`. Their
 * `framekeep-core` lives inside the installed package directory, which a
 * separate Node process has no business reaching -- and until S6 the adapter
 * called core on every path anyway, including the one where the app had
 * already answered. `video_map` and `video_frames` both failed with
 * CoreNotFound for everyone who was not us.
 *
 * So: with core pointed at nothing, an app that has the recording must be
 * enough. These run against a stand-in app on a real socket, and the whole
 * point of the assertions is that no core is involved in reaching them.
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
  home = fs.mkdtempSync(path.join(os.tmpdir(), 'framekeep-nocore-'));
  process.env.USERPROFILE = home;
  process.env.HOME = home;
  // The stranger's machine: whatever core is, it is not here.
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

/** An app holding one reviewed recording, with two of its frames masked. */
function appWithRecording(suffix: string, frameDir: string): Promise<void> {
  const address =
    process.platform === 'win32'
      ? `\\\\.\\pipe\\framekeep-v1-nocore-${process.pid}-${suffix}`
      : path.join(home, `framekeep-v1-${suffix}.sock`);

  const reply = {
    handle: 'abcdef0123456789',
    source_path: 'C:/videos/demo.mp4',
    video: { width: 1624, height: 860, duration_ms: 24_127 },
    transcript: {
      status: 'ready',
      has_audio: true,
      model: 'ggml-base.bin',
      created_unix: 1_780_000_000,
      segments: [{ start_seconds: 0, end_seconds: 3, text: 'the part that matters' }],
    },
    frames: [
      { pts_time: 0, file: path.join(frameDir, 'redacted', 'frame-00001.webp'), redacted: true },
      { pts_time: 5, file: path.join(frameDir, 'frame-00002.webp'), redacted: false },
    ],
    frame_count: 2,
    review: { reviewed_at: 1_780_000_100, found: 3, hidden: 1 },
  };

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
            ? {
                result: {
                  capabilities: ['queue', 'ingest', 'frames', 'redaction'],
                  version: '0.1.0',
                  protocol: 1,
                },
              }
            : { result: reply };
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

function textOf(result: { content: { type: string; text?: string }[] }): string {
  return result.content.find((c) => c.type === 'text')?.text ?? '';
}

describe('an installed app is enough on its own', () => {
  it('maps a recording with no core anywhere', async () => {
    await appWithRecording('map', path.join(home, 'cache'));
    const { videoMap } = await import('../src/tools/video_map.js');

    const text = textOf(await videoMap({ path: 'C:/videos/demo.mp4' }));

    // Size and length came from the app's own row, and the words from the
    // transcript it already produced. Reaching core for any of it is what
    // used to break this.
    expect(text).toContain('1624x860');
    expect(text).toContain('24s');
    expect(text).toContain('the part that matters');
  });

  it('sends the frames, and sends the masked ones masked', async () => {
    const frameDir = path.join(home, 'cache');
    fs.mkdirSync(path.join(frameDir, 'redacted'), { recursive: true });
    // Three distinguishable payloads, and the third is the point: the ORIGINAL
    // of the frame somebody masked. Without it on disk, "we sent the masked
    // copy" and "we sent the only file there was" look identical.
    const MASKED = Buffer.alloc(64, 1);
    const PLAIN = Buffer.alloc(64, 2);
    const ORIGINAL = Buffer.alloc(64, 7);
    fs.writeFileSync(path.join(frameDir, 'redacted', 'frame-00001.webp'), MASKED);
    fs.writeFileSync(path.join(frameDir, 'frame-00001.webp'), ORIGINAL);
    fs.writeFileSync(path.join(frameDir, 'frame-00002.webp'), PLAIN);
    await appWithRecording('frames', frameDir);
    const { videoFrames } = await import('../src/tools/video_frames.js');

    const result = (await videoFrames(
      { path: 'C:/videos/demo.mp4', from: 0, to: 30 },
      undefined,
    )) as {
      content: { type: string; text?: string; data?: string }[];
      structuredContent?: Record<string, unknown>;
    };

    // Asserted on the bytes that left, not on a path that says which bytes
    // ought to have. The reply no longer carries paths at all in this mode --
    // and a test that had gone on reading them would have been checking a
    // label instead of the goods.
    const sent = result.content.filter((c) => c.type === 'image').map((c) => c.data);
    expect(sent).toContain(MASKED.toString('base64'));
    expect(sent).not.toContain(ORIGINAL.toString('base64'));
    expect(sent).toContain(PLAIN.toString('base64'));

    // And said out loud, so the model knows without inspecting anything.
    const listed = result.structuredContent?.frames as { pts_time: number; redacted: boolean }[];
    expect(listed.find((f) => f.pts_time === 0)?.redacted).toBe(true);
    expect(listed.find((f) => f.pts_time === 5)?.redacted).toBe(false);
  });

  it('still refuses a recording nobody has reviewed', async () => {
    // The gate does not get more permissive just because core is out of the
    // picture. This is the one behaviour that must survive every change here.
    const address =
      process.platform === 'win32'
        ? `\\\\.\\pipe\\framekeep-v1-nocore-${process.pid}-gate`
        : path.join(home, 'framekeep-v1-gate.sock');
    const server = net.createServer((socket) => {
      socket.setEncoding('utf8');
      socket.on('data', (chunk: string) => {
        for (const line of String(chunk).split('\n').filter(Boolean)) {
          const request = JSON.parse(line) as { id: string; method: string };
          const body =
            request.method === 'hello'
              ? { result: { capabilities: ['queue', 'frames'], version: '0.1.0', protocol: 1 } }
              : {
                  error: {
                    code: 'AWAITING_REVIEW',
                    message: 'This recording is waiting for your review in Framekeep.',
                  },
                };
          socket.write(`${JSON.stringify({ id: request.id, ...body })}\n`);
        }
      });
    });
    servers.push(server);
    await new Promise<void>((resolve) =>
      server.listen(address, () => {
        fs.writeFileSync(addressFile(), address);
        resolve();
      }),
    );

    const { videoFrames } = await import('../src/tools/video_frames.js');
    const text = textOf(await videoFrames({ path: 'C:/videos/demo.mp4' }, undefined));
    expect(text).toContain('waiting for your review');
  });
});
