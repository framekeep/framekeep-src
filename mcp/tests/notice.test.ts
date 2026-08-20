/**
 * The warning has one job: never let anyone believe redaction ran when it did
 * not. These check what it says in each state, and that it survives the trip to
 * the model down both channels -- prose sent beside `structuredContent`
 * disappears on Claude Code, which is exactly where this line would be needed.
 */

import { describe, expect, it } from 'vitest';
import { redactionNotice } from '../src/notice.js';
import { bothChannels } from '../src/channels.js';

describe('the redaction notice', () => {
  it('says nothing was scanned, because nothing was', () => {
    const lines = redactionNotice({ appPresent: false, scanned: false, hidden: false });
    expect(lines.join(' ')).toContain('nothing was scanned for secrets');
    expect(lines.join(' ')).toContain('nothing was hidden');
    // The claim we must not make until S5 exists.
    expect(lines.join(' ')).not.toContain('secrets were detected');
  });

  it('does not tell someone to install what they already have', () => {
    const installed = redactionNotice({ appPresent: true, scanned: false, hidden: false });
    expect(installed.join(' ')).not.toContain('Install the app');
    expect(installed.join(' ')).toContain('Paste it into Framekeep');
    // And it is still a warning: this recording went nowhere near the app.
    expect(installed.join(' ')).toContain('nothing was hidden');
  });

  it('uses the mandated wording once something has actually scanned', () => {
    // S5's state, wired now so the copy does not have to be rediscovered later.
    const lines = redactionNotice({ appPresent: false, scanned: true, hidden: false, found: 2 });
    expect(lines).toEqual([
      'Running without Framekeep app — secrets were detected but not hidden.',
      'Install the app for redaction review.',
    ]);
  });

  it('stays quiet when the content really was redacted', () => {
    // A warning on every single reply is a warning nobody reads by the third one.
    expect(redactionNotice({ appPresent: true, scanned: true, hidden: true })).toEqual([]);
  });

  /**
   * The measurement behind `channels.ts`: one client keeps the text and drops
   * the structured half, another does the reverse. A warning that only rides
   * one of them reaches half the users.
   */
  it('reaches the model whichever channel the client keeps', () => {
    const notice = redactionNotice({ appPresent: false, scanned: false, hidden: false });
    const result = bothChannels({
      instructions: [...notice, '11 frames were selected.'],
      data: { frame_count: 11 },
    });

    const text = result.content
      .filter((c: { type: string }) => c.type === 'text')
      .map((c: { text?: string }) => c.text ?? '')
      .join('\n');
    expect(text).toContain('nothing was scanned for secrets');

    expect(JSON.stringify(result.structuredContent)).toContain('nothing was scanned for secrets');
  });

  it('comes before anything the model is meant to act on', () => {
    const notice = redactionNotice({ appPresent: false, scanned: false, hidden: false });
    const instructions = [...notice, 'Mapped a 42s recording.'];
    expect(instructions[0]).toContain('Running without Framekeep app');
  });
});
