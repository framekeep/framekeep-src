/**
 * Finding the app, or not finding it fast. S3.7.
 *
 * The case that has to be quick is the one where Framekeep is not installed --
 * most people trying this will have only the MCP server -- so "no app" must
 * cost nothing rather than 300 ms on every call.
 */

import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import net from 'node:net';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

import { addressFile, connect, CONNECT_TIMEOUT_MS } from '../src/tray.js';

let home: string;
let originalHome: string | undefined;
let servers: net.Server[] = [];

beforeEach(() => {
  originalHome = process.env.USERPROFILE ?? process.env.HOME;
  home = fs.mkdtempSync(path.join(os.tmpdir(), 'framekeep-tray-test-'));
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

function publish(address: string) {
  fs.writeFileSync(addressFile(), address);
}

function testAddress(suffix: string): string {
  return process.platform === 'win32'
    ? `\\\\.\\pipe\\framekeep-v1-test-${process.pid}-${suffix}`
    : path.join(home, `framekeep-v1-${suffix}.sock`);
}

/** A stand-in for the app: speaks the same JSON Lines, answers `hello`. */
function fakeTray(address: string, reply: (method: string) => unknown): Promise<void> {
  const server = net.createServer((socket) => {
    let buffer = '';
    socket.setEncoding('utf8');
    socket.on('data', (chunk: string) => {
      buffer += chunk;
      for (let i = buffer.indexOf('\n'); i >= 0; i = buffer.indexOf('\n')) {
        const line = buffer.slice(0, i);
        buffer = buffer.slice(i + 1);
        const request = JSON.parse(line) as { id: string; method: string };
        socket.write(`${JSON.stringify({ id: request.id, ...(reply(request.method) as object) })}\n`);
      }
    });
  });
  servers.push(server);
  return new Promise((resolve) => server.listen(address, () => resolve()));
}

describe('looking for the app', () => {
  it('costs nothing at all when there is no app', async () => {
    const started = Date.now();
    const attempt = await connect();
    const spent = Date.now() - started;

    expect(attempt.connected).toBe(false);
    if (!attempt.connected) expect(attempt.reason).toContain('not running');
    // The budget exists for the case where something is listening. Spending it
    // on every call for people who never installed the app is the failure this
    // asserts against.
    expect(spent).toBeLessThan(CONNECT_TIMEOUT_MS);
  });

  it('gives up at the deadline when the address leads nowhere', async () => {
    publish(testAddress('nobody-home'));
    const started = Date.now();
    const attempt = await connect(150);
    const spent = Date.now() - started;

    expect(attempt.connected).toBe(false);
    if (!attempt.connected) expect(attempt.reason).toMatch(/did not answer|not running/);
    expect(spent).toBeLessThan(2000);
  });

  it('will not talk to an address that is not one of ours', async () => {
    // A stale or edited file pointing at some other program's socket. Sending
    // it a `hello` would be rude, and reading its answer would be worse.
    publish(path.join(home, 'not-a-framekeep-thing.txt'));
    const attempt = await connect();
    expect(attempt.connected).toBe(false);
    if (!attempt.connected) expect(attempt.reason).toContain('does not look like one');
  });

  it('reads what the app can do rather than guessing from its version', async () => {
    const address = testAddress('working');
    await fakeTray(address, () => ({
      result: { server: 'framekeep-tray', version: '9.9.9', protocol: 1, capabilities: ['queue'] },
    }));
    publish(address);

    const attempt = await connect();
    expect(attempt.connected).toBe(true);
    if (attempt.connected) {
      expect(attempt.tray.capabilities).toEqual(['queue']);
      expect(attempt.tray.version).toBe('9.9.9');
      attempt.tray.close();
    }
  });

  it('falls back rather than crashing when the app speaks a different protocol', async () => {
    const address = testAddress('mismatch');
    await fakeTray(address, () => ({
      error: { code: 'PROTOCOL_MISMATCH', message: 'This Framekeep speaks protocol 2.' },
    }));
    publish(address);

    const attempt = await connect();
    expect(attempt.connected).toBe(false);
    if (!attempt.connected) expect(attempt.reason).toContain('PROTOCOL_MISMATCH');
  });

  it('surfaces a refusal with its code, so callers can tell FORBIDDEN from a failure', async () => {
    const address = testAddress('forbidden');
    await fakeTray(address, (method) =>
      method === 'hello'
        ? { result: { protocol: 1, capabilities: ['queue'] } }
        : { error: { code: 'FORBIDDEN', message: 'Only you can add a recording to Framekeep.' } },
    );
    publish(address);

    const attempt = await connect();
    expect(attempt.connected).toBe(true);
    if (attempt.connected) {
      await expect(attempt.tray.call('video.ingest')).rejects.toMatchObject({
        code: 'FORBIDDEN',
      });
      attempt.tray.close();
    }
  });
});
