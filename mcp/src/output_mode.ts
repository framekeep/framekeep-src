/**
 * How the caller wants frames delivered -- S2.4.
 *
 * Route A is images in the reply. It is the good path and the default, and it
 * dies at a hard edge: the reply cap counts pixel area (`budget.ts`), so past
 * a certain frame size no picture fits at all. Route B is paths on disk for the
 * caller to open itself, which the cap does not apply to.
 *
 * The plan's rule for `auto` is "try A, fall to B when it breaks, and do NOT
 * sniff the client". That split is kept here on purpose:
 *
 *   - what is *mechanically* impossible is decided by us. One frame that
 *     cannot fit the ceiling means route A returns a truncated reply, which
 *     research 2.1 describes as dying silently -- so `auto` steps down.
 *   - what a *client* can render is decided by the caller, by naming a mode.
 *     We do not guess it from `clientInfo`; a table of client abilities goes
 *     stale on their next release, and `channels.ts` already paid for that
 *     lesson once.
 *
 * # Why `files` is refused for redacted recordings
 *
 * The masked copy of a frame is written next to the frame it masks -- one
 * directory below the original (`tray/src-tauri/src/review.rs`). Route A never
 * exposes that: the model receives pixels, not a location. Route B is
 * *defined* as handing over a location to open.
 *
 * So on a recording where a person hid something, route B is a step away from
 * the version they hid it from. `video_frames` already refuses `region` on
 * exactly this reasoning -- a crop would be taken from the originals, and the
 * honest move is to refuse rather than quietly hand back unmasked pixels. This
 * is the same rule meeting the same situation through a different door.
 */

export type OutputMode = 'auto' | 'images' | 'files' | 'text';

/** What `auto` is allowed to resolve to. `auto` never survives resolution. */
export type Delivery = 'images' | 'files' | 'text';

const MODES: readonly OutputMode[] = ['auto', 'images', 'files', 'text'];

/**
 * Reads the caller's mode, or says why it cannot.
 *
 * Refuses an unknown value rather than falling back to `auto`. A caller who
 * typed `output_mode: "image"` wants something specific, and silently giving
 * them the default means they never learn the name was wrong.
 */
export function parseMode(v: unknown): OutputMode | { error: string } {
  if (v === undefined || v === null) return 'auto';
  if (typeof v !== 'string' || !MODES.includes(v as OutputMode)) {
    return {
      error:
        `output_mode must be one of ${MODES.join(', ')}; got ${JSON.stringify(v)}. ` +
        'Leave it out to get images.',
    };
  }
  return v as OutputMode;
}

/**
 * Why route B is closed for this recording, or null when it is open.
 *
 * Worded for the model that has to act on it. It says what it cannot have and
 * what to ask for instead -- and deliberately does not describe where the
 * originals sit, because that sentence would be the map it is refusing to draw.
 */
export function filesRefusal(hiddenFrames: number): string | null {
  if (hiddenFrames <= 0) return null;
  return (
    'This recording has frames where the person hid something, and `output_mode: files` ' +
    'is not available for those. Ask for images instead -- you get the frames with ' +
    'what they hid still hidden.'
  );
}

export interface Conditions {
  /** What one frame of the size being sent costs, in reply tokens. */
  frameTokens: number;
  /** Room for images in one reply, after prose is reserved. */
  ceiling: number;
  /** How many frames of this recording carry masks. */
  hiddenFrames: number;
}

export interface Resolution {
  delivery: Delivery;
  /** Set when `auto` stepped down; the model is told this rather than left to infer it. */
  steppedDown: string | null;
}

/**
 * Turns the mode the caller asked for into the one this reply will use.
 *
 * Only `auto` ever changes. A caller who names `images` gets images even when
 * they will not fit -- with the shortfall reported, as it already was. Choosing
 * something other than what was asked for, silently, is the failure this whole
 * file is arranged against.
 */
export function resolveMode(asked: OutputMode, c: Conditions): Resolution {
  if (asked !== 'auto') return { delivery: asked, steppedDown: null };

  // Everything fits the ordinary way. This is almost always the answer: a
  // full-HD frame costs ~2.8k of a ~23k ceiling.
  if (c.frameTokens <= c.ceiling) return { delivery: 'images', steppedDown: null };

  // One frame cannot fit. Sending it anyway is not a smaller answer, it is a
  // reply the client cuts -- the model receives nothing and cannot tell.
  const why =
    `A single frame at this size costs about ${c.frameTokens} tokens, more than the ` +
    `${c.ceiling} this reply has room for, so the picture cannot be sent whole.`;

  const closed = filesRefusal(c.hiddenFrames);
  if (!closed) {
    return {
      delivery: 'files',
      steppedDown: `${why} Returning paths instead, for you to open yourself.`,
    };
  }

  // Route A is too big and route B is closed. Say both, and hand back the one
  // thing left that is true: what is in there and when.
  return {
    delivery: 'text',
    steppedDown:
      `${why} ${closed} Listing the frames instead. Narrow \`from\`/\`to\` and pass ` +
      '`region` to crop to part of the screen -- a smaller area does fit.',
  };
}
