/**
 * How many frames actually fit in one reply.
 *
 * Claude Code caps MCP tool output at about 25k tokens. Over the cap the reply
 * is cut and the model stops receiving it -- Route A dies quietly, which is the
 * failure research 2.1 warned about.
 *
 * # The unit is pixel area, not bytes
 *
 * The first version of this file counted base64 bytes. That was a guess, and
 * measurement killed it (`docs/experiments/mcp-output-cap.md`). Two payloads
 * built to pull in opposite directions:
 *
 *     20 x 360x360 noise PNG    10 MB of base64,  2.6 M px   -> arrived intact
 *     16 x 1920x1080 interface  266 KB of base64, 33 M px    -> reply truncated
 *
 * Eight megabytes more base64 arrived fine. So bytes are not what is counted.
 * Area is: the break sits exactly between 9 and 10 full-HD frames, which is
 * where `(w*h)/750` crosses 25,000.
 *
 * # Two things this changes
 *
 * WebP lossless does NOT buy more frames. It is 3.9x fewer bytes for the same
 * pixels, and the cap does not care about bytes -- PNG and WebP both fit nine.
 * It is still worth having for disk and I/O, but it is not the lever.
 *
 * The lever is area. Cropping (`region`) reduces area without losing a pixel of
 * what is kept, so it is the honest way to fit more. Downscaling also reduces
 * area, and is still refused -- it blurs the text this product exists to make
 * readable, which is selling the thing we are selling.
 */

/** Token budget per reply, per client. */
const BUDGETS: { match: RegExp; tokens: number; why: string }[] = [
  {
    // Measured: 9 frames of 1920x1080 arrive intact, 10 begin to be cut.
    match: /claude/i,
    tokens: 25_000,
    why: 'Claude Code cuts tool output at about 25k tokens',
  },
];

/**
 * Used when `clientInfo` names a client we have not measured.
 *
 * Same as the lowest known cap. Guessing high means the model silently receives
 * nothing; guessing low means it receives fewer frames and is told so. Those
 * two costs are not equal.
 */
const UNKNOWN_CLIENT_TOKENS = 25_000;

/**
 * Room left for everything that is not an image: the instructions, the frame
 * list, the transcript when it rides along. Reserved rather than hoped for --
 * at 16 frames the measured failure was the *text* disappearing while the
 * images arrived, so prose is what gets squeezed out first.
 */
const PROSE_RESERVE_TOKENS = 2_000;

export interface Budget {
  tokens: number;
  client: string;
  why: string;
}

export function budgetFor(clientName: string | undefined): Budget {
  const name = clientName ?? 'unknown';
  const known = BUDGETS.find((b) => b.match.test(name));
  if (known) return { tokens: known.tokens, client: name, why: known.why };
  return {
    tokens: UNKNOWN_CLIENT_TOKENS,
    client: name,
    why: 'client not measured; using the lowest budget we know of',
  };
}

/**
 * What an image costs, in tokens.
 *
 * `(w*h)/750` predicts the measured break between 9 and 10 full-HD frames
 * almost exactly. It is an approximation that fits the observation -- not a
 * claim about how any provider bills.
 */
export function imageTokens(width: number, height: number): number {
  return Math.ceil((width * height) / 750);
}

/**
 * Room actually available for images, once prose has its share.
 *
 * Exported because `output_mode` has to answer a question the packer cannot:
 * whether even ONE frame fits. The packer's job is to stop at the edge; the
 * mode's job is to notice the edge is below the first item and take another
 * route entirely (`output_mode.ts`).
 */
export function ceilingFor(budget: Budget): number {
  return Math.max(0, budget.tokens - PROSE_RESERVE_TOKENS);
}

export interface Packed<T> {
  included: T[];
  /** How many were asked for but left out. */
  omitted: number;
  tokensUsed: number;
}

/**
 * Fills the budget in order, stopping before the first item that would not fit.
 *
 * Stops rather than skipping ahead to a smaller item: frames are a sequence in
 * time, and a reply that quietly jumps from second 4 to second 40 because the
 * middle frame was larger would misrepresent the recording. Leaving a contiguous
 * run and saying where it ended is honest; cherry-picking is not.
 */
export function packWithinBudget<T>(
  items: T[],
  costOf: (item: T) => number,
  budget: Budget,
): Packed<T> {
  const ceiling = ceilingFor(budget);
  const included: T[] = [];
  let used = 0;

  for (const item of items) {
    const cost = costOf(item);
    if (used + cost > ceiling) break;
    included.push(item);
    used += cost;
  }

  // Never return nothing. One frame over budget still beats a reply with no
  // picture in it, and the caller is told the budget was exceeded.
  if (included.length === 0 && items.length > 0) {
    const first = items[0] as T;
    included.push(first);
    used = costOf(first);
  }

  return { included, omitted: items.length - included.length, tokensUsed: used };
}
