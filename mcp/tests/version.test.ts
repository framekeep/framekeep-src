import { describe, expect, it } from 'vitest';
import { spawn } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

/**
 * The version the server tells clients has to be the version npm published.
 *
 * It was a literal in `index.ts` and it drifted: the server announced 0.1.0
 * while the registry served 0.2.0, for two releases, and nothing failed --
 * which is exactly why it lasted. A wrong version number costs nothing until
 * somebody is trying to work out which build they are looking at, and by then
 * they are debugging the wrong code.
 *
 * Asked of the built server over the real protocol rather than of the source,
 * because reading `VERSION` out of a TypeScript file would only prove the
 * source agrees with itself. What ships is `dist/`, and what a client sees is
 * an `initialize` reply.
 */
const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = join(HERE, '..');

const PACKAGE_VERSION: string = JSON.parse(
  readFileSync(join(ROOT, 'package.json'), 'utf8'),
).version;

/** Speaks `initialize` to the built server and returns its `serverInfo`. */
function askServerInfo(): Promise<{ name: string; version: string }> {
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [join(ROOT, 'dist', 'index.js')], { stdio: 'pipe' });
    let buffered = '';
    let stderr = '';

    const done = (err?: Error, info?: { name: string; version: string }) => {
      try {
        child.kill();
      } catch {
        /* already gone */
      }
      if (err) reject(err);
      else resolve(info!);
    };

    child.stdout.setEncoding('utf8');
    child.stdout.on('data', (chunk) => {
      buffered += chunk;
      let nl: number;
      while ((nl = buffered.indexOf('\n')) !== -1) {
        const line = buffered.slice(0, nl).trim();
        buffered = buffered.slice(nl + 1);
        if (!line) continue;
        try {
          const message = JSON.parse(line);
          if (message.id === 1 && message.result?.serverInfo) {
            done(undefined, message.result.serverInfo);
            return;
          }
        } catch {
          /* not one of ours */
        }
      }
    });

    child.stderr.setEncoding('utf8');
    child.stderr.on('data', (d) => (stderr += d));
    // A server that dies on an import never answers; say so rather than
    // waiting out the timeout with no explanation.
    child.on('exit', () => done(new Error(`the server exited. stderr:\n${stderr}`)));

    child.stdin.write(
      JSON.stringify({
        jsonrpc: '2.0',
        id: 1,
        method: 'initialize',
        params: {
          protocolVersion: '2025-06-18',
          capabilities: {},
          clientInfo: { name: 'framekeep-version-test', version: '1.0.0' },
        },
      }) + '\n',
    );

    setTimeout(() => done(new Error(`no initialize reply. stderr:\n${stderr}`)), 10_000);
  });
}

describe('the version the server announces', () => {
  it('is the version this package was published as', async () => {
    const info = await askServerInfo();
    expect(info.version).toBe(PACKAGE_VERSION);
  }, 15_000);

  it('is not a literal left behind in the build', () => {
    const built = readFileSync(join(ROOT, 'dist', 'index.js'), 'utf8');
    // The drift survived review because a literal reads as harmless. Any
    // version-shaped string sitting in the built entry point is the shape of
    // that mistake coming back, whatever number it happens to hold today.
    expect(built).not.toMatch(/version:\s*['"]\d+\.\d+\.\d+['"]/);
  });
});
