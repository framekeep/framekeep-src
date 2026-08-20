/**
 * The line that says what was done about secrets before this reply left. S3.8.
 *
 * `AGENTS.md` calls this mandatory and explains why in one sentence: if someone
 * believes redaction ran when it did not, they will send an API key to a cloud
 * model because of a decision made here.
 *
 * It goes into `instructions` -- the one field Framekeep speaks in its own voice
 * -- and therefore down both channels. It is prose, and prose sent beside
 * `structuredContent` vanishes on Claude Code; that measurement is the whole
 * reason `channels.ts` exists.
 *
 * # Why the wording is not the wording in copy.md, yet
 *
 * `_design_system/copy.md` mandates:
 *
 *     Running without Framekeep app — secrets were detected but not hidden.
 *
 * That sentence promises a scan ran. Redaction is S5 and nothing scans anything
 * today, so shipping it now would claim a capability this build does not have,
 * in the direction that sounds more careful than we are. A user reading "secrets
 * were detected" concludes Framekeep looked; it did not.
 *
 * So the mandated wording is kept for the state it describes, and the states
 * that are true today get sentences that are true today. When S5 lands, the
 * `scanned` branch below is already the copy.md line.
 */

export interface Situation {
  /** Did the Framekeep app answer on the socket? */
  appPresent: boolean;
  /** Did anything look at this recording for secrets? False until S5. */
  scanned: boolean;
  /** Were the findings hidden before this reply left? */
  hidden: boolean;
  /** How many were found, when something looked. */
  found?: number;
}

/**
 * Nothing at all when the content was reviewed and redacted -- a warning that
 * fires every time is a warning nobody reads by the third call.
 */
export function redactionNotice(s: Situation): string[] {
  if (s.hidden) return [];

  if (s.scanned) {
    // The mandated copy, for the state it was written about.
    return [
      'Running without Framekeep app — secrets were detected but not hidden.',
      'Install the app for redaction review.',
    ];
  }

  if (s.appPresent) {
    // The app is here, so telling them to install it would be nonsense. What
    // is true is narrower and more useful: *this recording* never went through
    // it, because it was read straight off a path.
    return [
      "This recording didn't go through Framekeep — nothing was scanned for secrets, and nothing was hidden.",
      'Paste it into Framekeep to review it before sending anything on.',
    ];
  }

  return [
    'Running without Framekeep app — nothing was scanned for secrets, and nothing was hidden.',
    'Install the app to review recordings before they reach chat.',
  ];
}
