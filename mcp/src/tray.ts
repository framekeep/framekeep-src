/**
 * Finding the Framekeep app, or deciding quickly that it is not there. S3.7.
 *
 * # Why a file, not a computed address
 *
 * On Windows the pipe is `\\.\pipe\framekeep-v1-<your SID>`, and Node has no way
 * to read this account's SID. Shelling out to `whoami` would mean spawning a
 * process and parsing localised output on a 300 ms budget, on the platform this
 * product bets on. So the app -- which already knows the answer -- writes it to
 * `~/.framekeep/ipc-address`, and this reads it.
 *
 * That also makes the common case free rather than cheap: no file means no app,
 * and the adapter knows without spending a millisecond of the budget. Most
 * people trying Framekeep will have installed only this.
 *
 * # What this is not
 *
 * The address file is a hint, not a credential. Anything running as this user
 * could rewrite it -- and could equally run `framekeep-core` itself, so there is
 * nothing to protect here that is not already reachable. The shape check below
 * is to catch a stale or corrupted file, not an attacker.
 */

import net from 'node:net';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

/**
 * The plan's number, and it is a budget rather than a guess: a person waiting
 * on a tool call should never pay a visible price for software they did not
 * install.
 */
export const CONNECT_TIMEOUT_MS = 300;

/** Cheap calls only, for now. Nothing here waits on ffmpeg or whisper. */
const CALL_TIMEOUT_MS = 10_000;

/** Bumped together with the tray's `PROTOCOL`; a mismatch means standalone. */
const PROTOCOL = 1;

export function addressFile(): string {
  return path.join(os.homedir(), '.framekeep', 'ipc-address');
}

export interface Tray {
  /** What the app says it can do. Read this; never infer it from a version. */
  capabilities: string[];
  version: string;
  call<T>(method: string, params?: unknown): Promise<T>;
  close(): void;
}

export type Attempt =
  | { connected: true; tray: Tray }
  | { connected: false; reason: string };

/**
 * An address only counts if it looks like one of ours.
 *
 * A stale file pointing at some other program's pipe would otherwise get a
 * `hello` from us, which is rude at best.
 */
function plausible(address: string): boolean {
  if (address.length === 0 || address.length > 512) return false;
  if (process.platform === 'win32') return /^\\\\[.?]\\pipe\\framekeep-v\d+-/.test(address);
  return address.includes('framekeep') && address.endsWith('.sock');
}

export async function connect(timeoutMs = CONNECT_TIMEOUT_MS): Promise<Attempt> {
  let address: string;
  try {
    address = fs.readFileSync(addressFile(), 'utf8').trim();
  } catch {
    return { connected: false, reason: 'the Framekeep app is not running' };
  }
  if (!plausible(address)) {
    return { connected: false, reason: 'the app left an address that does not look like one' };
  }

  const socket = await open(address, timeoutMs);
  if (!socket) {
    return { connected: false, reason: 'the app did not answer in time' };
  }

  const pending = new Map<string, (reply: Reply) => void>();
  let buffer = '';
  let closed = false;

  socket.setEncoding('utf8');
  socket.on('data', (chunk: string) => {
    buffer += chunk;
    for (let i = buffer.indexOf('\n'); i >= 0; i = buffer.indexOf('\n')) {
      const line = buffer.slice(0, i);
      buffer = buffer.slice(i + 1);
      let reply: Reply;
      try {
        reply = JSON.parse(line) as Reply;
      } catch {
        continue; // A line we cannot read is not a reply we can deliver.
      }
      const resolve = pending.get(String(reply.id));
      if (resolve) {
        pending.delete(String(reply.id));
        resolve(reply);
      }
    }
  });

  // A dropped connection resolves everything still waiting, rather than leaving
  // a tool call hanging until its own timeout.
  const drop = () => {
    closed = true;
    for (const [id, resolve] of pending) {
      pending.delete(id);
      resolve({ id, error: { code: 'CORE_FAILED', message: 'Framekeep closed the connection.' } });
    }
  };
  socket.on('error', drop);
  socket.on('close', drop);

  let next = 0;
  const send = (method: string, params: unknown) =>
    new Promise<Reply>((resolve) => {
      if (closed) {
        resolve({ id: '-', error: { code: 'CORE_FAILED', message: 'Framekeep is no longer connected.' } });
        return;
      }
      const id = String(next++);
      const timer = setTimeout(() => {
        pending.delete(id);
        resolve({ id, error: { code: 'CORE_FAILED', message: `Framekeep did not answer ${method}.` } });
      }, CALL_TIMEOUT_MS);
      pending.set(id, (reply) => {
        clearTimeout(timer);
        resolve(reply);
      });
      socket.write(`${JSON.stringify({ id, method, params: params ?? {} })}\n`);
    });

  const hello = await send('hello', {
    client: 'framekeep-mcp',
    version: '0.1.0',
    protocol: PROTOCOL,
  });
  if (hello.error) {
    socket.destroy();
    // PROTOCOL_MISMATCH lands here, and standalone is exactly the right answer
    // to it: one of the two is older, and neither should crash over that.
    return { connected: false, reason: `the app speaks a different version (${hello.error.code})` };
  }

  const result = (hello.result ?? {}) as { capabilities?: string[]; version?: string };
  return {
    connected: true,
    tray: {
      capabilities: result.capabilities ?? [],
      version: result.version ?? 'unknown',
      close: () => socket.destroy(),
      async call<T>(method: string, params?: unknown): Promise<T> {
        const reply = await send(method, params);
        if (reply.error) throw new TrayRefused(reply.error.code, reply.error.message);
        return reply.result as T;
      },
    },
  };
}

/** An error the app returned, with its code kept so callers can branch on it. */
export class TrayRefused extends Error {
  constructor(
    public readonly code: string,
    message: string,
  ) {
    super(message);
    this.name = 'TrayRefused';
  }
}

interface Reply {
  id: string | number;
  result?: unknown;
  error?: { code: string; message: string };
}

/** Connect, or give up at the deadline. Never throws. */
function open(address: string, timeoutMs: number): Promise<net.Socket | null> {
  return new Promise((resolve) => {
    const socket = net.connect(address);
    const done = (value: net.Socket | null) => {
      clearTimeout(timer);
      socket.removeAllListeners('connect');
      socket.removeAllListeners('error');
      if (!value) socket.destroy();
      resolve(value);
    };
    const timer = setTimeout(() => done(null), timeoutMs);
    socket.once('connect', () => done(socket));
    socket.once('error', () => done(null));
  });
}
