/**
 * `video_frames` -- the pictures themselves.
 *
 * The frames already exist on disk; core wrote them during `video_map`. This
 * reads them and fits as many as the client's reply budget allows.
 *
 * That budget is counted in pixel area, not bytes -- measured, see `budget.ts`.
 * Nine full-HD frames reach a Claude Code reply intact; a tenth starts cutting
 * the prose off the end.
 */

import { readFile } from 'node:fs/promises';
import * as core from '../core.js';
import { bothChannels, type ToolMessage } from '../channels.js';
import { noticeFor } from './video_map.js';
import { reviewedFrames } from '../reviewed.js';
import { budgetFor, ceilingFor, imageTokens, packWithinBudget, type Budget } from '../budget.js';
import {
  filesRefusal,
  parseMode,
  resolveMode,
  type Delivery,
  type OutputMode,
} from '../output_mode.js';

export const DESCRIPTION = [
  'Return actual frames from a screen recording, as images you can look at.',
  '',
  'Call video_map FIRST to see what is in the recording and when. Then call',
  'this with a narrow time range around the moment you care about.',
  '',
  'About nine full-screen frames fit in one reply. Asking for a wide range does',
  'not get you more -- it gets you the beginning of it, and a note saying what',
  'was left out. A narrow range gets you the moment you wanted.',
  '',
  'If you only care about part of the screen -- one panel, a dialog, an error',
  'message -- pass `region` as [x1,y1,x2,y2] in the recording\'s own pixels.',
  'The crop is full resolution, nothing is shrunk, and a smaller area means',
  'several times more frames fit in the same reply. Use video_map to see the',
  'recording\'s width and height before choosing a region.',
  '',
  'Do NOT call this before video_map, and do NOT ask for the whole recording.',
  '',
  // Named here rather than discovered in a reply: a model that meets `files`
  // for the first time as a fallback tends to read it as the tool failing.
  'You get images unless you ask otherwise. `output_mode` changes that:',
  '  images  the frames themselves. The default, and what you usually want.',
  '  files   paths on this machine for you to open yourself. The reply-size',
  '          limit does not apply, so this is how you reach frames that did',
  '          not fit -- at the cost of reading each one.',
  '  text    no pictures: only which frames exist and when. The cheapest way',
  '          to check whether the moment you want is even in the recording.',
  '`files` is not available for recordings where the person hid something. Ask',
  'for images there -- what they hid stays hidden.',
  '',
  // Two things a model gets wrong when it meets them for the first time in a
  // reply rather than in the description: it retries a refusal that will never
  // change, and it reports deliberate masking as a damaged image.
  'A recording may be waiting for the person to review it. Then you get no',
  'frames and a sentence saying so -- that is the product working. Retrying',
  'changes nothing; tell them to open Framekeep and approve it.',
  '',
  'Frames may arrive with black rectangles over parts of the screen. Those are',
  'deliberate: the person hid something before letting the frame through. The',
  'image is not damaged, there is nothing to recover, and what is underneath is',
  'not yours to ask for.',
].join('\n');

export interface FramesArgs {
  path: string;
  from?: number;
  to?: number;
  max_frames?: number;
  /** `[x1, y1, x2, y2]` in the recording's own pixels. */
  region?: number[];
  /** `auto | images | files | text`. Validated here, not trusted. */
  output_mode?: unknown;
}

/** An upper bound so a caller cannot ask for a thousand; the budget decides the rest. */
const MAX_FRAMES_CEILING = 20;

export async function videoFrames(args: FramesArgs, clientName: string | undefined) {
  // Rejected before the gate is even consulted, because a misspelled mode is
  // the caller's mistake and answering it with a review notice would send them
  // looking in the wrong place entirely.
  const asked = parseMode(args.output_mode);
  if (typeof asked !== 'string') return failure(asked.error);

  // Before any bytes are read. This tool is the one that actually sends
  // pictures, so it is the one place the review gate has to hold.
  const state = await reviewedFrames(args.path);
  if (state.kind === 'awaiting') {
    return bothChannels({
      instructions: [state.message, 'Nothing to retry until then -- this is by design.'],
      data: { path: args.path, awaiting_review: true, shown: 0 },
    });
  }

  // The app already extracted these frames and already knows how big they
  // are. Asking core for the same list is asking twice, and it makes core a
  // requirement of the ordinary path -- which is where the loop broke for
  // anyone who installed the app rather than building it, since an installed
  // app keeps core inside its package directory.
  //
  // The substitution below still runs, and still matches on `pts_time`: it is
  // now matching the app's list against itself, which is exactly right and
  // costs nothing.
  const map =
    state.kind === 'reviewed'
      ? {
          path: args.path,
          video: { width: state.video.width, height: state.video.height },
          frames: state.frames.map((f) => ({ pts_time: f.pts_time, file: f.file })),
        }
      : await core.mapFramesOnly(args.path);
  const budget = budgetFor(clientName);

  const from = args.from ?? 0;
  const to = args.to ?? Number.POSITIVE_INFINITY;
  const ceiling = Math.min(args.max_frames ?? MAX_FRAMES_CEILING, MAX_FRAMES_CEILING);

  const inRange = map.frames.filter((f) => f.pts_time >= from && f.pts_time <= to);
  const requested = inRange.slice(0, ceiling);

  if (requested.length === 0) {
    return bothChannels({
      instructions: [
        `No frames fall between ${from}s and ${fmt(to)}.`,
        `This recording has ${map.frames.length} frames; call video_map to see when they are.`,
      ],
      data: { path: map.path, shown: 0, available: map.frames.length, from, to },
    });
  }

  // A region changes both what is sent and what it costs, so it has to be
  // resolved before anything is measured.
  const region = parseRegion(args.region);
  if (region && 'error' in region) return failure(region.error);

  let files = requested;
  let width = map.video.width ?? 1920;
  let height = map.video.height ?? 1080;
  let areaKept: number | null = null;

  // The substitution the whole bridge exists for: where a person masked a
  // frame, the masked copy is the file that gets read. Matched by pts_time,
  // which is core's own identifier for a frame and survives re-encoding and
  // renaming -- the same key the crop path below matches on.
  if (state.kind === 'reviewed' && state.frames.length > 0) {
    const masked = new Map(state.frames.map((f) => [f.pts_time, f]));
    files = requested.map((f) => {
      const m = masked.get(f.pts_time);
      return m ? { pts_time: f.pts_time, file: m.file } : f;
    });
  }

  const hiddenFrames = state.kind === 'reviewed' ? state.hiddenFrames : 0;
  const maskedTimes = new Set(
    state.kind === 'reviewed' ? state.frames.filter((f) => f.redacted).map((f) => f.pts_time) : [],
  );

  // Answered before core is touched and before a byte is read, because `text`
  // promises no pictures and producing pictures is not free: cropping writes
  // new files to the user's disk. A region is reported by arithmetic instead.
  if (asked === 'text') {
    return bothChannels(
      listing(
        {
          notice: noticeFor(state),
          steppedDown: null,
          path: map.path,
          frames: files,
          masked: maskedTimes,
          available: inRange.length,
          from,
          to,
        },
        {
          width: region ? region.x2 - region.x1 : map.video.width,
          height: region ? region.y2 - region.y1 : map.video.height,
          cropped: region !== null,
        },
      ),
    );
  }

  // Route B hands over a location to open. On a recording where somebody hid
  // something, a location is one step from the version they hid it from --
  // the same reasoning the `region` refusal below already runs on, arriving
  // through a different door.
  //
  // Refused for the whole recording rather than per frame: returning only the
  // frames nobody masked would quietly drop exactly the moments they cared
  // about, and a short list reads like a short recording.
  if (asked === 'files') {
    const closed = filesRefusal(hiddenFrames);
    if (closed) return failure(closed);
  }

  if (region) {
    // A crop of a reviewed recording would come from the ORIGINAL frames --
    // core crops from the path it is given, and it knows nothing about masks.
    // Cropping a masked frame is not implemented, so the honest move is to
    // refuse rather than quietly hand back unmasked pixels.
    if (state.kind === 'reviewed' && state.hiddenFrames > 0) {
      return failure(
        'This recording has redacted frames, and `region` would crop the originals. ' +
          'Ask again without `region` to get the redacted frames whole.',
      );
    }
    const cropped = await core.crop(args.path, region);
    // core returns the crops in the same order as the source frames, so the
    // time range already chosen still applies.
    const byTime = new Map(cropped.frames.map((f) => [f.pts_time, f.file]));
    files = requested
      .map((f) => ({ pts_time: f.pts_time, file: byTime.get(f.pts_time) ?? f.file }))
      .filter((f) => byTime.has(f.pts_time));
    width = cropped.region.width;
    height = cropped.region.height;
    areaKept = cropped.area_kept_percent;
  }

  // Every frame here is the same size, so they all cost the same: no need to
  // decode each one to find out how much room it takes.
  const costEach = imageTokens(width, height);

  // What this reply will actually carry. Only `auto` moves here, and when it
  // moves the reply says so -- a model that silently gets paths where it
  // expected pictures reports the tool as broken.
  const resolution = resolveMode(asked, {
    frameTokens: costEach,
    ceiling: ceilingFor(budget),
    hiddenFrames,
  });

  const common = {
    notice: noticeFor(state),
    steppedDown: resolution.steppedDown,
    path: map.path,
    frames: files,
    masked: maskedTimes,
    available: inRange.length,
    from,
    to,
  };

  if (resolution.delivery === 'files') {
    return bothChannels(paths(common, { width, height, areaKept, costEach }));
  }
  if (resolution.delivery === 'text') {
    return bothChannels(listing(common, { width, height, cropped: region !== null }));
  }

  // core already wrote these as lossless WebP; this only reads them.
  const encoded = await Promise.all(
    files.map(async (f) => ({
      pts_time: f.pts_time,
      file: f.file,
      bytes: await readFile(f.file),
    })),
  );

  const packed = packWithinBudget(encoded, () => costEach, budget);
  const dropped = inRange.length - packed.included.length;

  return bothChannels(
    message({ ...common, frames: packed.included }, dropped, budget, {
      tokensUsed: packed.tokensUsed,
      costEach,
      width,
      height,
      areaKept,
    }),
    packed.included.map((e) => ({
      data: e.bytes.toString('base64'),
      mimeType: 'image/webp',
    })),
  );
}

/** What every reply carries, whichever route it takes. */
interface Common {
  notice: string[];
  /** Set when `auto` changed route; goes right after the notice, never before it. */
  steppedDown: string | null;
  path: string;
  frames: { pts_time: number; file: string }[];
  /** pts_time of every frame a person masked. */
  masked: Set<number>;
  available: number;
  from: number;
  to: number;
}

/**
 * The notice keeps the first line in every reply.
 *
 * It is the sentence saying what was done about secrets, and `AGENTS.md` calls
 * it mandatory. A route change is worth telling the model about, but not worth
 * pushing the safety line down the page for.
 */
function opening(c: Common): string[] {
  return [...c.notice, ...(c.steppedDown ? [c.steppedDown] : [])];
}

/**
 * One entry per frame.
 *
 * `redacted` is stated rather than left to be read off the path. A model that
 * has to infer masking from a directory name is a model we have taught to look
 * at directory names.
 *
 * # Why the path is only in one route
 *
 * A location on disk is what `output_mode: files` exists to hand over, and it
 * is refused outright where anything was hidden. Everywhere else the reply
 * already carries the pixels, so the path buys the caller nothing -- while
 * still sitting one directory from the frames a person masked.
 *
 * `video_map` reached this same answer first and quietly: it has always listed
 * frames as `{index, pts_time}` and never as a place to look.
 */
function entries(c: Common, withPaths: boolean) {
  return c.frames.map((f, index) => ({
    index,
    pts_time: f.pts_time,
    ...(withPaths ? { file: f.file } : {}),
    redacted: c.masked.has(f.pts_time),
  }));
}

/**
 * Route B: paths, no pixels.
 *
 * The reply cap counts image area, and this reply carries no images, so the
 * packer is not involved -- every frame the caller asked for is listed. What
 * replaces the cap is a stated price: reading them all costs about this much,
 * and the caller is the one who can decide whether to pay it.
 */
function paths(
  c: Common,
  size: { width: number; height: number; areaKept: number | null; costEach: number },
): ToolMessage {
  const what = size.areaKept === null ? 'full frame' : `${size.width}x${size.height} crop`;
  const all = size.costEach * c.frames.length;
  return {
    instructions: [
      ...opening(c),
      `${c.frames.length} ${what}${c.frames.length === 1 ? '' : 's'} from ${fmt(c.from)} to ` +
        `${fmt(c.to)}, as paths on this machine. Open the ones you need.`,
      `Each costs about ${size.costEach} tokens to look at, so all ${c.frames.length} is ` +
        `roughly ${all}. Reading them all at once is usually not what you want.`,
    ],
    data: {
      path: c.path,
      output_mode: 'files',
      shown: c.frames.length,
      available: c.available,
      truncated_because: null,
      frames: entries(c, true),
      encoding: 'image/webp (lossless)',
      frame_size: { width: size.width, height: size.height },
      area_kept_percent: size.areaKept,
      tokens_per_frame: size.costEach,
      tokens_if_all_read: all,
    },
  };
}

/**
 * The cheapest answer: what is in the range, and when.
 *
 * No pixels and no paths. It exists so a model can find the right moment before
 * paying for it, and so there is still something true to return when a frame is
 * too large to send and route B is closed.
 */
function listing(
  c: Common,
  size: { width: number | null; height: number | null; cropped: boolean },
): ToolMessage {
  const size_ = size.width && size.height ? `${size.width}x${size.height}` : 'unknown size';
  const hidden = c.frames.filter((f) => c.masked.has(f.pts_time)).length;
  const instructions = [
    ...opening(c),
    `${c.frames.length} frame${c.frames.length === 1 ? '' : 's'} between ${fmt(c.from)} and ` +
      `${fmt(c.to)}, at ${c.frames.map((f) => `${f.pts_time.toFixed(1)}s`).join(', ')}.` +
      ` Each is ${size_}${size.cropped ? ' after the crop you asked for' : ''}.`,
    'No pictures in this reply -- you asked for a listing. Call again with ' +
      '`output_mode: images` and a narrow from/to to see them.',
  ];
  if (hidden > 0) {
    instructions.push(
      `${hidden} of them carry parts the person hid. Those parts stay hidden in the images too.`,
    );
  }
  return {
    instructions,
    data: {
      path: c.path,
      output_mode: 'text',
      shown: 0,
      available: c.available,
      truncated_because: null,
      frames: entries(c, false),
      frame_size: { width: size.width, height: size.height },
      redacted_frames: hidden,
    },
  };
}

interface Sizing {
  tokensUsed: number;
  costEach: number;
  width: number;
  height: number;
  /** Percentage of the full frame kept, when a region was applied. */
  areaKept: number | null;
}

/**
 * Route A: the pictures themselves.
 *
 * `c.frames` here is what survived the packer, not what was asked for -- the
 * gap between the two is `dropped`, and saying it out loud is the whole reason
 * this function is longer than a template string.
 */
function message(c: Common, dropped: number, budget: Budget, sizing: Sizing): ToolMessage {
  const shown = c.frames;
  const what = sizing.areaKept === null ? 'full frame' : `${sizing.width}x${sizing.height} crop`;
  const instructions = [
    ...opening(c),
    `${shown.length} ${what}${shown.length === 1 ? '' : 's'} from ${fmt(c.from)} to ${fmt(c.to)}, ` +
      `at ${shown.map((s) => `${s.pts_time.toFixed(1)}s`).join(', ')}.`,
  ];

  // Never let the model believe it has seen everything when it has not. A reply
  // that quietly stops at the budget looks exactly like a complete one.
  if (dropped > 0) {
    instructions.push(
      `Showing ${shown.length} of ${c.available} frames in this range -- the rest did not fit ` +
        `in one reply (${budget.why}). The frames continue after ${last(shown).toFixed(1)}s.`,
    );
    // The way out that costs nothing, named rather than left to be guessed:
    // the cap counts pixel area, so a smaller area is more frames.
    instructions.push(
      sizing.areaKept === null
        ? 'Either narrow from/to, or pass `region` to crop to the part of the screen ' +
          'you care about -- a crop is full resolution, and a smaller area fits ' +
          'several times more frames in one reply.'
        : 'Narrow from/to, or tighten `region` further.',
    );
    // The route that has no cap at all. Offered only when it is actually open:
    // on a redacted recording `files` is refused, and naming it there would be
    // telling the model to ask for something it will not get.
    if (filesRefusal(c.masked.size) === null) {
      instructions.push(
        'Or pass `output_mode: files` to get every frame in the range as a path, ' +
          'and open only the ones you need.',
      );
    }
  }

  return {
    instructions,
    data: {
      path: c.path,
      output_mode: 'images',
      shown: shown.length,
      available: c.available,
      truncated_because: dropped > 0 ? 'reply_token_budget' : null,
      frames: entries(c, false),
      encoding: 'image/webp (lossless)',
      frame_size: { width: sizing.width, height: sizing.height },
      area_kept_percent: sizing.areaKept,
      tokens_used: sizing.tokensUsed,
      tokens_per_frame: sizing.costEach,
      tokens_budget: budget.tokens,
      client: budget.client,
    },
  };
}

/**
 * Turns the caller's four numbers into a rectangle, or says why it cannot.
 *
 * Refuses rather than repairs. A caller who asked about the wrong part of the
 * screen should be told, not handed a different part and left to wonder why the
 * answer does not match the question.
 */
function parseRegion(r: number[] | undefined): core.Region | { error: string } | null {
  if (r === undefined) return null;
  if (!Array.isArray(r) || r.length !== 4 || r.some((n) => typeof n !== 'number')) {
    return { error: 'region must be four numbers: [x1, y1, x2, y2] in the recording\'s pixels.' };
  }
  const [x1, y1, x2, y2] = r as [number, number, number, number];
  if (x2 <= x1 || y2 <= y1) {
    return {
      error: `region needs x2 > x1 and y2 > y1; got [${r.join(', ')}].`,
    };
  }
  return { x1: Math.round(x1), y1: Math.round(y1), x2: Math.round(x2), y2: Math.round(y2) };
}

function failure(message: string) {
  return {
    ...bothChannels({ instructions: [message], data: { error: message } }),
    isError: true,
  };
}

const fmt = (n: number) => (Number.isFinite(n) ? `${n.toFixed(1)}s` : 'the end');
const last = (xs: { pts_time: number }[]) => xs[xs.length - 1]!.pts_time;
