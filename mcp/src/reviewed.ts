/**
 * Asking the app what a person decided about this recording.
 *
 * The adapter can always read a video itself -- that is the standalone path,
 * and it says plainly that nothing was reviewed. What it cannot work out alone
 * is which frames a person masked. Only the app knows, so only the app is
 * asked, and the answer decides which files this reply is allowed to carry.
 *
 * Three outcomes, and keeping them apart is the whole point:
 *
 *   reviewed  the app has this recording, a person approved it, and the frame
 *             list it returns is already the redacted one where anything was
 *             hidden. Use those paths, not core's.
 *   awaiting  the app has it and a person has NOT approved it. Nothing is
 *             served -- the model is told to go and ask them.
 *   absent    the app is not running, or never saw this file. Read it directly
 *             and warn that review was skipped. Not an error: most people
 *             trying Framekeep install only the MCP server.
 *
 * The connection is opened and closed per call, like `warnAboutSecrets`: the
 * app can start or stop between two tool calls, and a cached answer sends
 * somebody to the wrong place.
 */

import type { TranscriptStatus } from './core.js';
import { connect, TrayRefused } from './tray.js';

export interface ReviewedFrame {
  pts_time: number;
  /** The file to read. Already the masked copy where one exists. */
  file: string;
  redacted: boolean;
}

export type Reviewed =
  | {
      kind: 'reviewed';
      frames: ReviewedFrame[];
      /** How many findings a person saw, and how many frames carry masks. */
      found: number | null;
      hiddenFrames: number;
      /**
       * Everything else the app already knows, so this path never needs core.
       *
       * That is the whole point of carrying them. Until S6 they were fetched
       * from `framekeep-core` even when the app had answered, which quietly
       * made core a requirement of the ordinary path -- and an installed app
       * keeps its core inside the package directory, somewhere a separate
       * Node process has no business reaching. The loop did not close for
       * anyone who had not built the repo.
       */
      video: AppVideo;
      transcript: TranscriptStatus;
    }
  | { kind: 'awaiting'; message: string }
  | { kind: 'absent'; appPresent: boolean };

/** What the queue row knows about the recording. Milliseconds, as stored. */
export interface AppVideo {
  width: number | null;
  height: number | null;
  duration_ms: number | null;
}

interface TrayMap {
  frames?: ReviewedFrame[];
  review?: { found?: number | null; hidden?: number };
  video?: AppVideo;
  transcript?: TranscriptStatus;
}

export async function reviewedFrames(path: string): Promise<Reviewed> {
  const attempt = await connect();
  if (!attempt.connected) return { kind: 'absent', appPresent: false };

  try {
    // Older apps answered NOT_READY here; newer ones say so in `hello`. Asking
    // the capability list rather than the version is the rule this protocol was
    // built on -- a version number invites guessing about what it implies.
    if (!attempt.tray.capabilities.includes('frames')) {
      return { kind: 'absent', appPresent: true };
    }

    const map = await attempt.tray.call<TrayMap>('video.map', { path });
    return {
      kind: 'reviewed',
      frames: map.frames ?? [],
      found: map.review?.found ?? null,
      hiddenFrames: map.review?.hidden ?? 0,
      video: map.video ?? { width: null, height: null, duration_ms: null },
      // An app old enough to answer `video.map` without a transcript field is
      // saying nothing about the words, which is `absent` -- not "no speech".
      transcript: map.transcript ?? { status: 'absent' },
    };
  } catch (e) {
    if (e instanceof TrayRefused) {
      // The gate. A person has not looked at this yet, and no amount of
      // retrying will change that -- the message says so, because a bare
      // refusal makes models try variations of the same call.
      if (e.code === 'AWAITING_REVIEW') return { kind: 'awaiting', message: e.message };
      // NOT_FOUND is the ordinary case: a path nobody pasted into the app.
      // Anything else -- a broken queue, a method this build lacks -- lands
      // here too, and the honest answer is the same: read it directly and say
      // review was skipped. Guessing further would be pretending to know.
      return { kind: 'absent', appPresent: true };
    }
    return { kind: 'absent', appPresent: true };
  } finally {
    attempt.tray.close();
  }
}
