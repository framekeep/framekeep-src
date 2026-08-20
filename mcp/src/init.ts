/**
 * `framekeep-mcp init` -- write the client config so nobody has to.
 *
 * # The one thing this command exists to avoid
 *
 * On Windows, `spawn("npx", args)` fails with ENOENT and `npx.cmd` fails with
 * EINVAL -- Node refuses both (`docs/experiments/npx-spawn-windows.md`). Some
 * clients paper over it by putting a shell in the path; Claude Code 2.1.223 was
 * measured doing exactly that. Others were not measured, and any of them could
 * change on their next release.
 *
 * So the config this writes never contains the word `npx`. It points at
 * `node` and an absolute path, which every client can spawn.
 *
 * That is the whole trick behind the one-line install: `npx framekeep-mcp init`
 * is typed by a person into a terminal, where a shell exists and npx works
 * fine. The command then removes npx from the daily path entirely. The promise
 * is kept without keeping the breaking point.
 *
 * # Why it writes project files by default
 *
 * A global config belongs to the user, not to us, and rewriting it is not the
 * sort of thing a tool should do because it seemed convenient. Project-scoped
 * files sit in the repo the user is already working in, are obvious in `git
 * status`, and are deleted by deleting them. `--global` exists, and it says
 * what it will do and refuses to do it without `--yes`.
 */

import { mkdirSync, readFileSync, writeFileSync, existsSync } from 'node:fs';
import { dirname, join, parse, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { homedir } from 'node:os';

const here = dirname(fileURLToPath(import.meta.url));

/**
 * The folder the user's editor has open, which is where client config belongs.
 *
 * Not the current directory. The install instructions say `cd mcp` before
 * building, so the obvious implementation wrote three config files into
 * `mcp/` -- where no client would ever look for them. Nothing failed, nothing
 * warned; the config simply had no effect. Walking up to the `.git` folder
 * finds the project whichever subdirectory someone happens to be standing in.
 *
 * Falls back to the current directory when there is no repository, and says
 * which it chose either way -- a tool that writes files somewhere should name
 * the somewhere.
 */
export function projectRoot(from: string): { dir: string; why: string } {
  let dir = resolve(from);
  const { root } = parse(dir);
  while (true) {
    if (existsSync(join(dir, '.git'))) {
      return { dir, why: 'the repository this folder belongs to' };
    }
    if (dir === root) break;
    dir = dirname(dir);
  }
  return { dir: resolve(from), why: 'the current folder -- no repository above it' };
}

/** The entry point this package was installed at, as an absolute path. */
function serverEntry(): string {
  return resolve(join(here, 'index.js'));
}

export interface Target {
  /** What the user calls it. */
  client: string;
  /** Where the file goes, relative to the project root. Absent when the client
   *  has no project-scoped config at all. */
  projectPath?: string;
  /** Where the file goes when `--global` is used, relative to home. */
  globalPath?: string;
  /** Config language. Codex is the odd one out. */
  syntax: 'json' | 'toml';
  /**
   * The key servers live under. VS Code uses `servers` and requires a `type`;
   * the others use `mcpServers`. Getting this wrong produces a client that
   * silently starts nothing, which reads exactly like a broken server.
   */
  key: 'mcpServers' | 'servers';
  needsType: boolean;
}

export const TARGETS: Target[] = [
  {
    client: 'Claude Code',
    projectPath: '.mcp.json',
    globalPath: '.claude.json',
    syntax: 'json',
    key: 'mcpServers',
    needsType: false,
  },
  {
    client: 'Cursor',
    projectPath: join('.cursor', 'mcp.json'),
    globalPath: join('.cursor', 'mcp.json'),
    syntax: 'json',
    key: 'mcpServers',
    needsType: false,
  },
  {
    client: 'VS Code',
    projectPath: join('.vscode', 'mcp.json'),
    syntax: 'json',
    key: 'servers',
    needsType: true,
  },
  {
    // Codex has no project-scoped config, so it is reachable only with
    // `--global` -- which is also the flag that makes us ask first, since that
    // file is the user's and shared by every project they open.
    client: 'Codex',
    globalPath: join('.codex', 'config.toml'),
    syntax: 'toml',
    key: 'mcpServers',
    needsType: false,
  },
];

/**
 * Adds or replaces one `[mcp_servers.framekeep]` section, leaving the rest of
 * the file byte for byte as it was.
 *
 * Deliberately not a TOML parse-and-rewrite, and not a new dependency. Parsing
 * the whole file and printing it back would reformat someone's config and drop
 * their comments -- a rude trade for a section we could have edited in place.
 * We only ever need to touch one section, so that is all this touches.
 */
export function mergeToml(existing: string | null, entry: string): { text: string; replaced: boolean } {
  const section = [
    '[mcp_servers.framekeep]',
    `command = ${tomlString(process.execPath)}`,
    'args = [',
    `    ${tomlString(entry)},`,
    ']',
  ].join('\n');

  const text = existing ?? '';
  // A section runs until the next header at the start of a line, or the end of
  // the file. Matching `[` only at column 0 keeps array values like `args = [`
  // from being mistaken for the next section.
  const start = text.search(/^\[mcp_servers\.framekeep\]\s*$/m);
  if (start === -1) {
    const padded = text.length === 0 || text.endsWith('\n\n') ? text : text.endsWith('\n') ? text + '\n' : text + '\n\n';
    return { text: padded + section + '\n', replaced: false };
  }

  const after = text.slice(start + 1);
  const nextHeader = after.search(/^\[/m);
  const end = nextHeader === -1 ? text.length : start + 1 + nextHeader;
  const rebuilt = text.slice(0, start) + section + '\n\n' + text.slice(end);
  // One trailing newline however many times this has run, so re-running init
  // does not slowly grow blank lines at the end of someone's config.
  return { text: rebuilt.replace(/\n+$/, '\n'), replaced: true };
}

/**
 * A TOML string that survives a Windows path.
 *
 * Literal strings do not process escapes, so `C:\Users\...` needs no doubling
 * and cannot be mangled by getting the doubling wrong. Falls back to a basic
 * string only if the path contains a quote, which no Windows path does.
 */
function tomlString(value: string): string {
  if (!value.includes("'")) return `'${value}'`;
  return JSON.stringify(value);
}

export function serverEntryFor(target: Target, entry: string): Record<string, unknown> {
  const spec: Record<string, unknown> = {
    command: process.execPath,
    args: [entry],
  };
  if (target.needsType) spec.type = 'stdio';
  return spec;
}

/**
 * Merges our server into whatever is already there.
 *
 * Reads, adds one key, writes back. A config file is the user's, and a tool
 * that replaces it wholesale would take out every other server they had set up
 * -- a rude way to be convenient.
 *
 * An unreadable file is left alone and reported, rather than overwritten: it
 * might be malformed, or it might be something we do not understand, and both
 * deserve a person looking rather than a tool guessing.
 */
export function mergeConfig(
  existing: string | null,
  target: Target,
  entry: string,
): { text: string; replaced: boolean } | { error: string } {
  let doc: Record<string, unknown> = {};
  if (existing !== null && existing.trim() !== '') {
    try {
      doc = JSON.parse(existing) as Record<string, unknown>;
    } catch {
      return {
        error:
          'That file exists but is not valid JSON, so it has been left alone. ' +
          'Fix or move it, then run init again.',
      };
    }
  }

  const servers = (doc[target.key] ?? {}) as Record<string, unknown>;
  const replaced = Object.prototype.hasOwnProperty.call(servers, 'framekeep');
  servers.framekeep = serverEntryFor(target, entry);
  doc[target.key] = servers;

  return { text: JSON.stringify(doc, null, 2) + '\n', replaced };
}

export interface Plan {
  client: string;
  file: string;
  action: 'create' | 'update' | 'replace' | 'skip';
  note?: string;
  text?: string;
}

/**
 * Narrows the run to one client.
 *
 * Added after watching `--global` offer to edit three personal config files
 * when only one of them was wanted. Touching more of someone's setup than they
 * asked for is not thoroughness.
 */
export function selectTargets(name: string | undefined): Target[] {
  if (!name) return TARGETS;
  const wanted = name.toLowerCase().replace(/[^a-z]/g, '');
  return TARGETS.filter((t) => t.client.toLowerCase().replace(/[^a-z]/g, '').includes(wanted));
}

export function planFor(root: string, global: boolean, targets: Target[] = TARGETS): Plan[] {
  const entry = serverEntry();
  const plans: Plan[] = [];

  for (const target of targets) {
    const rel = global ? target.globalPath : target.projectPath;
    if (!rel) {
      plans.push({
        client: target.client,
        file: '-',
        action: 'skip',
        note: global
          ? 'has no global config location; use the project-scoped file instead'
          : 'has no project-scoped config; run `init --global` to reach it',
      });
      continue;
    }
    const file = join(root, rel);
    const existing = existsSync(file) ? readFileSync(file, 'utf8') : null;
    const merged =
      target.syntax === 'toml' ? mergeToml(existing, entry) : mergeConfig(existing, target, entry);

    if ('error' in merged) {
      plans.push({ client: target.client, file, action: 'skip', note: merged.error });
      continue;
    }
    plans.push({
      client: target.client,
      file,
      action: existing === null ? 'create' : merged.replaced ? 'replace' : 'update',
      text: merged.text,
    });
  }
  return plans;
}

export function runInit(argv: string[]): number {
  const global = argv.includes('--global');
  const confirmed = argv.includes('--yes');

  const explicit = argv[argv.indexOf('--dir') + 1];
  const chosen =
    argv.includes('--dir') && explicit
      ? { dir: resolve(explicit), why: 'given with --dir' }
      : projectRoot(process.cwd());

  const only = argv.includes('--client') ? argv[argv.indexOf('--client') + 1] : undefined;
  const targets = selectTargets(only);
  if (targets.length === 0) {
    console.error(
      `No client matches "${only}". Known: ${TARGETS.map((t) => t.client).join(', ')}.`,
    );
    return 2;
  }

  const root = global ? homedir() : chosen.dir;
  const plans = planFor(root, global, targets);

  // Global edits are previewed and need a yes; project files are written on the
  // spot. Say which of the two is happening, in the tense it is happening in --
  // "would write" above a line saying "wrote 3 files" is a message that lies
  // about what it just did.
  const preview = global && !confirmed;

  console.log(`Framekeep MCP server: ${serverEntry()}`);
  console.log(
    preview
      ? `\nWould write to your user config in ${root}:`
      : global
        ? `\nWriting to your user config in ${root}:`
        : `\nWriting project config in ${root}\n  (${chosen.why}; override with --dir)`,
  );
  for (const p of plans) {
    const mark = p.action === 'skip' ? '  skip  ' : `  ${p.action.padEnd(7)}`;
    console.log(`${mark} ${p.client.padEnd(12)} ${p.file}`);
    if (p.note) console.log(`          ${p.note}`);
  }

  // Global config is the user's own, shared by every project they open. Saying
  // what will happen and waiting for a yes is the least a tool can do before
  // editing it.
  if (preview) {
    console.log(
      '\nNothing was written. This edits configuration outside this project;\n' +
        're-run with --yes to go ahead.',
    );
    return 0;
  }

  let wrote = 0;
  for (const p of plans) {
    if (p.action === 'skip' || !p.text) continue;
    mkdirSync(dirname(p.file), { recursive: true });
    writeFileSync(p.file, p.text, 'utf8');
    wrote += 1;
  }

  console.log(`\nWrote ${wrote} file${wrote === 1 ? '' : 's'}.`);
  console.log('Restart your client, then ask it to call video_map on a screen recording.');
  console.log(
    'The config points at node and an absolute path on purpose: on Windows a ' +
      'client that spawns a bare `npx` fails with ENOENT.',
  );
  return 0;
}
