/**
 * `video_map` -- everything about a recording except the pictures.
 *
 * Returns immediately. Speech takes ~110 seconds where frames take ~2.3, so
 * this never waits for words: it reports where the transcript has got to, kicks
 * transcription off if nobody has, and lets the caller come back.
 */

import * as core from '../core.js';
import { bothChannels, type ToolMessage } from '../channels.js';
import { redactionNotice } from '../notice.js';
import { connect } from '../tray.js';
import { reviewedFrames, type Reviewed } from '../reviewed.js';

export const DESCRIPTION = [
  'Map a screen recording: how long it is, what it contains, and which moments',
  'are worth looking at. Returns NO images -- it is the cheap first step.',
  '',
  'Call this FIRST, always. Then call video_frames for the few moments you',
  'actually need. Asking for every frame up front wastes the reply on pictures',
  'nobody looked at.',
  '',
  'Do NOT call this to browse a folder, to summarise a video, or on anything',
  'that is not a screen recording -- it decodes the file, which is not free.',
  '',
  'Speech is transcribed in the background and takes far longer than the frame',
  'map. If the transcript is not ready, call video_map again later; it is cheap',
  'and has no side effects.',
  '',
  // The gate, said in the description and not only in the refusal. A model
  // that meets AWAITING_REVIEW for the first time in a reply either retries in
  // a loop or reports the tool as broken; a model that was told about it up
  // front does the one useful thing, which is to ask the person.
  'A recording may be waiting for the person to review it. Then you get no',
  'frames and a sentence saying so. That is the product working, not a failure:',
  'retrying changes nothing, and the only thing that helps is telling them to',
  'open Framekeep and approve it.',
].join('\n');

export interface MapArgs {
  path: string;
}

/**
 * True when no process is producing a transcript for this recording.
 *
 * `absent` is nobody has asked yet. `running` with a stale lease is a process
 * that was working on it and is gone -- killed, or the machine slept. Exported
 * so the distinction can be tested: taking a stale lease at face value made
 * video_map report "transcribing, 65051s so far" for something that died
 * overnight, and nothing was going to finish it.
 */
export function nobodyIsWorking(status: core.TranscriptStatus): boolean {
  return status.status === 'absent' || (status.status === 'running' && status.stale);
}

/**
 * Ask the app whether it is there, and say what that means for this reply.
 *
 * Asked on every call rather than cached, because the app can start or stop
 * between two of them and a stale answer sends the user to the wrong place.
 * It is nearly free in the case that matters: with no app installed there is no
 * address file, and none of the 300 ms budget is spent.
 *
 * Used where nothing has been through the queue -- a path read straight off
 * disk. Where a recording HAS been reviewed, `noticeFor` below says so instead.
 */
export async function warnAboutSecrets(): Promise<string[]> {
  const attempt = await connect();
  if (attempt.connected) attempt.tray.close();
  return redactionNotice({
    appPresent: attempt.connected,
    scanned: false,
    hidden: false,
  });
}

/**
 * The notice that matches what actually happened to THIS recording.
 *
 * The silent case is the one worth naming: a reviewed recording gets no
 * warning at all. A caution printed on every reply is one nobody reads by the
 * third call, and here there is genuinely nothing to caution about -- a person
 * looked at every frame and approved what left.
 */
export function noticeFor(state: Reviewed): string[] {
  if (state.kind === 'reviewed') {
    return redactionNotice({
      appPresent: true,
      scanned: true,
      // A person reviewed it. Even with nothing masked, that is a decision
      // somebody made about these frames, not an absence of one.
      hidden: true,
      found: state.found ?? undefined,
    });
  }
  return redactionNotice({
    appPresent: state.kind === 'absent' ? state.appPresent : true,
    scanned: false,
    hidden: false,
  });
}

export async function videoMap(args: MapArgs) {
  // Ask the app first. A recording waiting on a person is refused here, before
  // anything is decoded -- the model is told to go and ask them rather than
  // handed a map of frames it may not see the pictures of.
  const state = await reviewedFrames(args.path);
  if (state.kind === 'awaiting') {
    return bothChannels({
      instructions: [state.message, 'Nothing to retry until then -- this is by design.'],
      data: { path: args.path, awaiting_review: true },
    });
  }

  // Two ways to be answered, and only one of them needs core.
  //
  // When the app has the recording it has everything: the frames it already
  // extracted, the words it already transcribed, the size it already probed.
  // Asking core again would be asking a second time for what is in hand -- and
  // worse, it would make core a hard requirement of the ordinary path, which
  // is how the loop failed to close for anyone who had not built this repo:
  // an installed app keeps its core inside the package directory, and a
  // separate Node process cannot reach in there.
  //
  // core stays exactly what it was for: the standalone path, for people who
  // installed the adapter and nothing else.
  const { map, status } =
    state.kind === 'reviewed'
      ? { map: fromApp(args.path, state), status: state.transcript }
      : await fromCore(args.path);

  const instructions = [
    // First, before anything the model might act on.
    ...noticeFor(state),
    `Mapped ${describe(map)}.`,
    `${map.frames.length} frames were selected as worth looking at.`,
    'Call video_frames with a time range to see any of them.',
  ];

  const message: ToolMessage = {
    instructions,
    data: {
      // `map.video` already carries the path core was given. Repeating it here
      // was free when only one channel shipped the data; now that both do, every
      // duplicated field is paid for twice.
      video: map.video,
      frames: map.frames.map((f, i) => ({ index: i, pts_time: f.pts_time })),
      frame_count: map.frames.length,
      selection: map.selection,
      // Said out loud so a caller can tell a reviewed recording from one read
      // off a path. `frames_redacted` is the count of frames carrying masks;
      // zero with `reviewed: true` means a person looked and hid nothing.
      reviewed: state.kind === 'reviewed',
      frames_redacted: state.kind === 'reviewed' ? state.hiddenFrames : 0,
      transcript: transcriptField(status, map.video.has_audio, instructions),
    },
  };

  if (status.status === 'ready' && status.segments.length > 0) {
    // The one part of this output composed by someone else. It goes in a
    // channel of its own, fenced, and the model is told to read rather than
    // obey it -- see src/channels.ts.
    message.extracted = [
      {
        kind: 'transcript',
        lines: status.segments.map(
          (s) => `[${s.start_seconds.toFixed(1)}s] ${s.text.trim()}`,
        ),
      },
    ];
  }

  return bothChannels(message);
}

/**
 * The app's answer, in the shape the rest of this file already renders.
 *
 * Adapted rather than rendered separately, so there is one description of a
 * recording and not two that drift. The fields the app genuinely does not
 * have are `null`, never invented: it stores no codec, no frame rate, and
 * nothing about HDR, because the pipeline had no reason to keep them.
 *
 * `selection` is absent for the same reason -- the thresholds belong to the
 * run that picked the frames, and repeating a plausible-looking set of
 * numbers here would be describing a decision nobody made.
 */
function fromApp(path: string, state: Extract<Reviewed, { kind: 'reviewed' }>): AppMap {
  const v = state.video;
  return {
    path,
    video: {
      duration_seconds: v.duration_ms == null ? null : v.duration_ms / 1000,
      width: v.width,
      height: v.height,
      codec: null,
      fps: null,
      // The transcript is the only thing that knows. Before it exists the
      // honest answer is "assume there is audio": saying `false` early would
      // print "this recording has no audio track" about one still being
      // transcribed.
      has_audio: state.transcript.status === 'ready' ? state.transcript.has_audio : true,
      is_hdr: false,
    },
    frames: state.frames.map((f) => ({ pts_time: f.pts_time, file: f.file })),
  };
}

/** The standalone path: read the file, and start the words if nobody has. */
async function fromCore(path: string): Promise<{ map: AppMap; status: core.TranscriptStatus }> {
  const map = await core.mapFramesOnly(path);
  const status = await core.transcriptStatus(path);

  // Two states mean the same thing: nobody is producing a transcript.
  //
  // `absent` is nobody has asked yet. `running` with a stale lease is a process
  // that was working on it and is gone -- killed, or the machine slept. core
  // flags that; taking it at face value would report "still transcribing" for
  // something that died hours ago, which is the silent kind of broken.
  //
  // Restarting is safe: core hands an expired lease to whoever asks next, so
  // the worst case is one wasted run, and the alternative is a recording whose
  // words never arrive and never say why.
  //
  // Only on this path. When the app owns the recording, the app owns the
  // transcript too, and a second process starting one behind its back is how
  // two writers end up in the same cache folder.
  if (nobodyIsWorking(status) && map.video.has_audio) {
    core.startTranscriptionInBackground(path);
  }
  return { map, status };
}

/** As much of a map as either source can honestly produce. */
type AppMap = Pick<core.VideoMap, 'path' | 'video' | 'frames'> &
  Partial<Pick<core.VideoMap, 'selection'>>;

function describe(map: AppMap): string {
  const v = map.video;
  const secs = v.duration_seconds ? `${v.duration_seconds.toFixed(0)}s` : 'unknown length';
  const size = v.width && v.height ? `${v.width}x${v.height}` : 'unknown size';
  return `a ${secs} ${size} recording${v.is_hdr ? ' (HDR)' : ''}`;
}

/**
 * Where the words have got to, and what the caller should do about it.
 *
 * Every branch says what to do next. "running" without an idea of how long is
 * the kind of status that makes a caller either give up or poll in a tight
 * loop, and both are our fault rather than theirs.
 */
function transcriptField(
  status: core.TranscriptStatus,
  hasAudio: boolean,
  instructions: string[],
): Record<string, unknown> {
  if (!hasAudio) {
    instructions.push('This recording has no audio track, so there is nothing to transcribe.');
    return { status: 'none', reason: 'the recording has no audio track' };
  }

  switch (status.status) {
    case 'ready':
      // The words themselves are deliberately NOT here. They go out once, in
      // `extracted_content`, fenced and labelled as written by someone else.
      //
      // The first version put them in both places, and the second copy was
      // raw: no fence, no warning, sitting in a field that reads like any
      // other piece of our own output. The injection test passed anyway --
      // the model saw the labelled copy too and believed that one -- which
      // made a hole look like a working guard. A duplicate of untrusted
      // content is a second chance for it to be trusted.
      return {
        status: 'ready',
        model: status.model,
        segment_count: status.segments.length,
        segments_are_in: 'extracted_content.transcript',
      };

    case 'running': {
      const waited = Math.max(0, Math.floor(Date.now() / 1000) - status.since_unix);
      if (status.stale) {
        // Reporting the age here would say "transcribing, 65051s so far" for a
        // process that died overnight. The age of a corpse is not progress.
        instructions.push(
          'A previous transcription of this recording stopped without finishing, ' +
            'so it has been started again. Call video_map later to collect it -- ' +
            'the frames above are already final.',
        );
        return { status: 'running', restarted: true, waiting_seconds: 0 };
      }
      instructions.push(
        `Speech is still being transcribed; ${waited}s of waiting so far. ` +
          'Call video_map again to collect it -- the frames above are already final.',
      );
      return { status: 'running', restarted: false, waiting_seconds: waited };
    }

    case 'failed':
      // Not retried automatically: a file whisper cannot read will not become
      // readable on the second attempt, and a tool that silently re-spends two
      // minutes every time it is called is worse than one that says what
      // happened. The way out is named instead of implied.
      instructions.push(
        'Speech could not be transcribed, and Framekeep will not retry on its own. ' +
          'The frames above are unaffected and still usable. ' +
          'To try again, run: framekeep-core transcribe "<path>" --fresh',
      );
      return { status: 'failed', error: status.error, retry_is_manual: true };

    case 'absent':
    default:
      instructions.push(
        'Speech transcription has just started. Call video_map again to collect it.',
      );
      return { status: 'running', restarted: true, waiting_seconds: 0 };
  }
}
