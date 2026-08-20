import { describe, expect, it } from 'vitest';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { mergeConfig, mergeToml, projectRoot, selectTargets, serverEntryFor, TARGETS } from '../src/init.js';

/** This repository, found from the test file rather than from cwd. */
const REPO = resolve(join(dirname(fileURLToPath(import.meta.url)), '..', '..'));

const claude = TARGETS.find((t) => t.client === 'Claude Code')!;
const vscode = TARGETS.find((t) => t.client === 'VS Code')!;
const cursor = TARGETS.find((t) => t.client === 'Cursor')!;

describe('the config never asks a client to spawn npx', () => {
  // This is the entire reason the command exists. On Windows a bare `npx` is
  // ENOENT and `npx.cmd` is EINVAL; one client was measured working around it,
  // and the others are a coin toss that can change on their next release.
  it('points at node and an absolute path', () => {
    for (const target of TARGETS) {
      const spec = serverEntryFor(target, 'C:/somewhere/dist/index.js');
      expect(JSON.stringify(spec)).not.toContain('npx');
      expect(spec.command).toBe(process.execPath);
      expect(spec.args).toEqual(['C:/somewhere/dist/index.js']);
    }
  });
});

describe('each client gets the shape it actually reads', () => {
  it('uses `servers` and a type for VS Code', () => {
    const out = mergeConfig(null, vscode, 'C:/x/index.js');
    const doc = JSON.parse(('text' in out ? out.text : '') || '{}');
    // Writing `mcpServers` here produces a client that starts nothing at all,
    // which reads exactly like a broken server.
    expect(doc.servers.framekeep.type).toBe('stdio');
    expect(doc.mcpServers).toBeUndefined();
  });

  it('uses `mcpServers` and no type for Claude Code and Cursor', () => {
    for (const target of [claude, cursor]) {
      const out = mergeConfig(null, target, 'C:/x/index.js');
      const doc = JSON.parse(('text' in out ? out.text : '') || '{}');
      expect(doc.mcpServers.framekeep.command).toBe(process.execPath);
      expect(doc.mcpServers.framekeep.type).toBeUndefined();
      expect(doc.servers).toBeUndefined();
    }
  });
});

describe('an existing config belongs to the user', () => {
  it('keeps every other server they had set up', () => {
    const existing = JSON.stringify({
      mcpServers: { somethingElse: { command: 'node', args: ['other.js'] } },
      unrelatedSetting: true,
    });
    const out = mergeConfig(existing, claude, 'C:/x/index.js');
    const doc = JSON.parse(('text' in out ? out.text : '') || '{}');

    expect(doc.mcpServers.somethingElse).toBeDefined();
    expect(doc.unrelatedSetting).toBe(true);
    expect(doc.mcpServers.framekeep).toBeDefined();
  });

  it('reports replacing our own entry rather than doing it quietly', () => {
    const existing = JSON.stringify({
      mcpServers: { framekeep: { command: 'node', args: ['old-path.js'] } },
    });
    const out = mergeConfig(existing, claude, 'C:/new/index.js');
    expect('text' in out && out.replaced).toBe(true);
    const doc = JSON.parse(('text' in out ? out.text : '') || '{}');
    expect(doc.mcpServers.framekeep.args).toEqual(['C:/new/index.js']);
  });

  it('leaves a file it cannot parse alone, and says so', () => {
    // It might be malformed, or it might be something we do not understand.
    // Both deserve a person looking rather than a tool overwriting.
    const out = mergeConfig('{ not json at all', claude, 'C:/x/index.js');
    expect('error' in out).toBe(true);
    expect('error' in out && out.error).toContain('left alone');
  });

  it('treats an empty file as a fresh start rather than an error', () => {
    const out = mergeConfig('   ', claude, 'C:/x/index.js');
    expect('text' in out).toBe(true);
  });
});

describe('choosing where the config goes', () => {
  it('walks up to the repository rather than trusting the current folder', () => {
    // The install instructions say `cd mcp` before building, so trusting cwd
    // wrote three config files into mcp/ where no client looks for them.
    // Nothing failed and nothing warned -- the config simply did nothing.
    const fromSubdir = projectRoot(join(REPO, 'mcp', 'src'));
    expect(fromSubdir.dir).toBe(REPO);
    expect(projectRoot(REPO).dir).toBe(REPO);
  });

  it('says which folder it picked and why', () => {
    expect(projectRoot(join(REPO, 'mcp')).why).toContain('repository');
  });
});

describe('Codex speaks TOML, and the rest of its file is not ours to reformat', () => {
  const codex = TARGETS.find((t) => t.client === 'Codex')!;
  const existing = [
    '[plugins."slack@openai-curated"]',
    'enabled = true',
    '',
    '[mcp_servers.codegraph]',
    'command = "codegraph"',
    'args = [',
    '    "serve",',
    '    "--mcp",',
    ']',
    '',
    '[desktop]',
    'external-agent-import-sync-enabled = true',
    '',
  ].join('\n');

  it('has no project-scoped config, only a global one', () => {
    // Codex is the one client that cannot be set up without touching a file
    // outside the project -- which is exactly the case --global guards.
    expect(codex.projectPath).toBeUndefined();
    expect(codex.globalPath).toBeDefined();
  });

  it('adds one section and leaves every other one untouched', () => {
    const out = mergeToml(existing, 'C:/x/dist/index.js');
    expect(out.replaced).toBe(false);
    expect(out.text).toContain('[mcp_servers.framekeep]');
    // Their servers, their plugins, their settings: all still there, verbatim.
    expect(out.text).toContain('[mcp_servers.codegraph]');
    expect(out.text).toContain('external-agent-import-sync-enabled = true');
    expect(out.text).toContain('[plugins."slack@openai-curated"]');
  });

  it('replaces its own section in place rather than adding a second one', () => {
    const once = mergeToml(existing, 'C:/old/index.js');
    const twice = mergeToml(once.text, 'C:/new/index.js');

    expect(twice.replaced).toBe(true);
    expect(twice.text.match(/\[mcp_servers\.framekeep\]/g)).toHaveLength(1);
    expect(twice.text).toContain('C:/new/index.js');
    expect(twice.text).not.toContain('C:/old/index.js');
    expect(twice.text).toContain('[desktop]');
  });

  it('does not mistake an array bracket for the next section', () => {
    // A section ends at the next header, and `args = [` is not one. A scan that
    // stopped at any `[` would cut a neighbour's array in half.
    const out = mergeToml(mergeToml(existing, 'C:/x/a.js').text, 'C:/x/b.js');
    expect(out.text).toContain('args = [\n    "serve",\n    "--mcp",\n]');
  });

  it('does not grow a blank line every time it runs', () => {
    let text = existing;
    for (let i = 0; i < 5; i += 1) text = mergeToml(text, `C:/x/${i}.js`).text;
    expect(text.endsWith('\n')).toBe(true);
    expect(text.endsWith('\n\n')).toBe(false);
    expect(text.match(/\[mcp_servers\.framekeep\]/g)).toHaveLength(1);
  });

  it('writes a Windows path without escaping it wrong', () => {
    // Built from a joined array rather than a literal: an earlier version of
    // this test lost its backslashes to the escape rules, so input and
    // expectation were mangled identically and it passed while testing nothing.
    const path = ['C:', 'Users', 'Nguyễn Văn A', 'dist', 'index.js'].join('\\');
    const out = mergeToml(null, path);

    expect(path).toContain('\\');
    // Literal strings pass backslashes through, so the path cannot be mangled
    // by getting the doubling wrong.
    expect(out.text).toContain(`'${path}'`);
    expect(out.text).not.toContain('\\\\');
  });

  it('never asks Codex to spawn npx either', () => {
    expect(mergeToml(null, 'C:/x/index.js').text).not.toContain('npx');
  });
});

describe('narrowing the run to one client', () => {
  it('matches on a rough name', () => {
    expect(selectTargets('codex').map((t) => t.client)).toEqual(['Codex']);
    expect(selectTargets('vs code').map((t) => t.client)).toEqual(['VS Code']);
    expect(selectTargets('CURSOR').map((t) => t.client)).toEqual(['Cursor']);
  });

  it('does everything when nothing is named', () => {
    expect(selectTargets(undefined)).toHaveLength(TARGETS.length);
  });

  it('matches nothing rather than guessing', () => {
    // Silently doing all four because the name was misspelled would edit three
    // config files nobody asked about.
    expect(selectTargets('emacs')).toHaveLength(0);
  });
});
