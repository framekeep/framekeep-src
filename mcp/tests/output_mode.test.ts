/**
 * The route decision, on its own -- S2.4.
 *
 * `video_frames` is awkward to drive end to end for these cases: reaching the
 * step-down needs a frame too large for any real recording we have. The rule
 * itself is arithmetic, so it is tested as arithmetic here, and the parts that
 * only appear in a real reply are tested against a real socket in
 * `frames_modes.test.ts`.
 */

import { describe, expect, it } from 'vitest';

import { filesRefusal, parseMode, resolveMode } from '../src/output_mode.js';
import { budgetFor, ceilingFor, imageTokens } from '../src/budget.js';

describe('reading the mode the caller asked for', () => {
  it('defaults to auto when nothing was asked', () => {
    expect(parseMode(undefined)).toBe('auto');
    expect(parseMode(null)).toBe('auto');
  });

  it('takes each of the four names', () => {
    for (const mode of ['auto', 'images', 'files', 'text']) {
      expect(parseMode(mode)).toBe(mode);
    }
  });

  it('refuses a near-miss instead of quietly giving the default', () => {
    // The failure this guards: `image` silently becoming `auto` means a caller
    // who wanted something specific gets the default forever and is never told
    // the name was wrong.
    const result = parseMode('image');
    expect(typeof result).not.toBe('string');
    expect((result as { error: string }).error).toContain('output_mode must be one of');
    expect((result as { error: string }).error).toContain('"image"');
  });

  it('refuses a value that is not even a string', () => {
    expect(typeof parseMode(3)).not.toBe('string');
    expect(typeof parseMode({ mode: 'files' })).not.toBe('string');
  });
});

describe('when route B is closed', () => {
  it('is open when the person hid nothing', () => {
    expect(filesRefusal(0)).toBeNull();
  });

  it('is closed as soon as one frame carries a mask', () => {
    expect(filesRefusal(1)).toContain('not available');
  });

  it('does not describe where the unmasked frames are', () => {
    // The refusal is read by a model with a filesystem. Naming the layout
    // would hand over the thing being withheld -- so the sentence must not
    // carry a directory, an extension, or the word for either.
    const message = filesRefusal(2) ?? '';
    expect(message).not.toMatch(/redacted\/|\.webp|director(y|ies)|folder|original/i);
    // ...and the check above is only meaningful if the message exists at all.
    expect(message.length).toBeGreaterThan(20);
  });
});

describe('auto picks a route, and says when it moved', () => {
  const budget = budgetFor('claude-code');
  const ceiling = ceilingFor(budget);

  it('sends images for an ordinary frame', () => {
    // A full-HD frame is ~2.8k against a ~23k ceiling. This is the answer
    // almost every real call gets.
    const r = resolveMode('auto', {
      frameTokens: imageTokens(1920, 1080),
      ceiling,
      hiddenFrames: 0,
    });
    expect(r.delivery).toBe('images');
    expect(r.steppedDown).toBeNull();
  });

  it('steps down to files when one frame cannot fit at all', () => {
    // 8K: (7680*4320)/750 is about 44k, well past the ceiling. Sending it is
    // not a smaller answer, it is a reply the client cuts.
    const frameTokens = imageTokens(7680, 4320);
    expect(frameTokens).toBeGreaterThan(ceiling);

    const r = resolveMode('auto', { frameTokens, ceiling, hiddenFrames: 0 });
    expect(r.delivery).toBe('files');
    expect(r.steppedDown).toContain('Returning paths instead');
  });

  it('steps down to text when the frame is too big AND route B is closed', () => {
    const r = resolveMode('auto', {
      frameTokens: imageTokens(7680, 4320),
      ceiling,
      hiddenFrames: 3,
    });
    expect(r.delivery).toBe('text');
    // Both halves of the reason, because either alone leaves the model
    // guessing at the other.
    expect(r.steppedDown).toContain('cannot be sent whole');
    expect(r.steppedDown).toContain('not available');
    // And the way out that does work.
    expect(r.steppedDown).toContain('region');
  });

  it('leaves a named mode alone, even when it will not fit', () => {
    // A caller who says `images` gets images. The shortfall is reported, as it
    // always was -- but choosing differently from what was asked, silently, is
    // the failure the whole file is arranged against.
    const frameTokens = imageTokens(7680, 4320);
    for (const mode of ['images', 'files', 'text'] as const) {
      const r = resolveMode(mode, { frameTokens, ceiling, hiddenFrames: 0 });
      expect(r.delivery).toBe(mode);
      expect(r.steppedDown).toBeNull();
    }
  });
});
