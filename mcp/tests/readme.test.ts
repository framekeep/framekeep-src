import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { TARGETS } from '../src/init.js';

/**
 * The npm page is the front door for anyone who did not arrive through the
 * repository, and it is the one surface where a wrong instruction costs the
 * most: someone follows it, nothing works, and there is no error to search
 * for because nothing failed.
 *
 * It said `Codex uses TOML rather than JSON; init prints the block to paste`.
 * `init` does not print a block. It skips Codex with a note pointing at
 * `--global`, so a Codex user waited for something that was never coming.
 * Nothing in the code could contradict the sentence, because prose does not
 * compile -- so these read the prose and compare it to `TARGETS`.
 */
const README = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), '..', 'README.md'),
  'utf8',
);

describe('the npm page describes the init that exists', () => {
  it('names the file each client actually gets', () => {
    const projectScoped = TARGETS.filter((t) => t.projectPath);
    expect(projectScoped.length).toBeGreaterThan(1);
    for (const target of projectScoped) {
      // Written with forward slashes in prose, joined with the platform
      // separator in code.
      const shown = target.projectPath!.split(/[\\/]/).join('/');
      expect(README, `${target.client} writes ${shown}`).toContain(shown);
      expect(README).toContain(target.client);
    }
  });

  it('sends the project-less client down the only route it has', () => {
    // Codex today; whoever else joins it tomorrow. A client with no
    // project-scoped path is unreachable without `--global`, and a page that
    // does not say so has told that client's users to run a command that
    // quietly skips them.
    const globalOnly = TARGETS.filter((t) => !t.projectPath && t.globalPath);
    for (const target of globalOnly) {
      const client = target.client.toLowerCase();
      expect(
        README.includes(`--client ${client}`),
        `${target.client} is reachable only with --global --client ${client}`,
      ).toBe(true);
      expect(README).toContain('--global');
    }
  });

  it('does not promise a block to paste', () => {
    // The exact shape of the sentence that was wrong. `init` writes files or
    // previews them; it never hands anyone something to copy by hand.
    expect(README.toLowerCase()).not.toContain('prints the block');
  });

  it('says the step that comes after approving', () => {
    // The mechanism people get wrong, and the reason the app grew a Copy
    // prompt button: approving unlocks a recording, it does not deliver one.
    // The model still has to be asked, and asked with a path.
    expect(README).toContain('Copy prompt');
    expect(README.toLowerCase()).toContain('path');
  });
});
