import { describe, expect, it } from 'vitest';
import { nobodyIsWorking } from '../src/tools/video_map.js';

describe('deciding whether anyone is producing a transcript', () => {
  it('nobody has asked yet', () => {
    expect(nobodyIsWorking({ status: 'absent' })).toBe(true);
  });

  it('a live lease means someone is on it -- do not start a second', () => {
    expect(
      nobodyIsWorking({ status: 'running', since_unix: 1_700_000_000, stale: false }),
    ).toBe(false);
  });

  it('a stale lease is a dead process, not progress', () => {
    // This is the case that shipped broken: the status said "running" and the
    // adapter reported 65051 seconds of progress for a two-minute recording
    // whose transcriber had died overnight. Nothing was going to finish it.
    expect(
      nobodyIsWorking({ status: 'running', since_unix: 1_700_000_000, stale: true }),
    ).toBe(true);
  });

  it('a recorded failure is left alone rather than retried forever', () => {
    // A file whisper cannot read will not become readable on the second
    // attempt, and silently re-spending two minutes per call is worse than
    // saying what happened.
    expect(nobodyIsWorking({ status: 'failed', error: 'x', at_unix: 1 })).toBe(false);
  });

  it('words already in hand need no work', () => {
    expect(
      nobodyIsWorking({
        status: 'ready',
        has_audio: true,
        model: 'tiny.en',
        created_unix: 1,
        segments: [],
      }),
    ).toBe(false);
  });
});
