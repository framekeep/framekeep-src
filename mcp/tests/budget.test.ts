import { describe, expect, it } from 'vitest';
import { budgetFor, imageTokens, packWithinBudget } from '../src/budget.js';

describe('the budget errs on the low side', () => {
  it('knows Claude Code', () => {
    const b = budgetFor('claude-code');
    expect(b.tokens).toBe(25_000);
    expect(b.why).toContain('25k tokens');
  });

  it('gives an unmeasured client the lowest budget it knows', () => {
    // Guessing high means the model silently receives nothing at all; guessing
    // low means it receives fewer frames and is told so. Not equal costs.
    expect(budgetFor('some-editor-2029').tokens).toBe(budgetFor('claude-code').tokens);
    expect(budgetFor(undefined).why).toContain('not measured');
  });
});

describe('what an image costs', () => {
  // The correction this file exists for: 10 MB of small images arrived intact
  // while 266 KB of large ones did not. See docs/experiments/mcp-output-cap.md.
  it('scales with area, and is blind to file size', () => {
    expect(imageTokens(1920, 1080)).toBeGreaterThan(imageTokens(360, 360) * 10);
  });

  it('straddles the measured break at nine full-HD frames', () => {
    const each = imageTokens(1920, 1080);
    expect(each * 9).toBeLessThan(25_000);
    expect(each * 10).toBeGreaterThan(25_000);
  });
});

describe('packing frames into a reply', () => {
  const budget = budgetFor('claude-code');
  const frame = (tokens: number) => ({ tokens });
  const cost = (f: { tokens: number }) => f.tokens;

  it('fits eight or nine full-HD frames, once prose is reserved', () => {
    // Nine fit on the wire. A frame's worth is held back so the instructions
    // and the truncation notice are not what gets cut -- at 16 frames the
    // measured failure was the text vanishing while the images arrived.
    const each = imageTokens(1920, 1080);
    const packed = packWithinBudget(Array.from({ length: 20 }, () => frame(each)), cost, budget);
    expect(packed.included.length).toBeGreaterThanOrEqual(8);
    expect(packed.included.length).toBeLessThanOrEqual(9);
    expect(packed.omitted).toBeGreaterThan(0);
  });

  it('stops at the first frame that does not fit, rather than skipping past it', () => {
    // Frames are a sequence in time. Jumping from second 4 to second 40 because
    // the frame in between was larger would misrepresent the recording.
    const items = [frame(100), frame(100), frame(30_000), frame(100)];
    const packed = packWithinBudget(items, cost, budget);
    expect(packed.included).toHaveLength(2);
    expect(packed.omitted).toBe(2);
  });

  it('always returns at least one frame, even over budget', () => {
    // A reply with no picture in it is worse than one that went over, and the
    // caller is told the budget was exceeded either way.
    const packed = packWithinBudget([frame(999_999)], cost, budget);
    expect(packed.included).toHaveLength(1);
    expect(packed.omitted).toBe(0);
  });

  it('handles an empty range without inventing a frame', () => {
    const packed = packWithinBudget([], cost, budget);
    expect(packed.included).toHaveLength(0);
    expect(packed.omitted).toBe(0);
  });
});
