/**
 * Send-feedback helpers.
 *
 * `buildFeedbackIssueUrl` and `buildFeedbackClipboardText` are pure functions
 * so they can be tested without a browser or a Tauri runtime.
 * `collectFeedbackContext` reads the app version and platform from Tauri when
 * the dialog asks for them, and `feedbackIssuesAreOpen` checks with GitHub
 * whether the issue form is worth opening at all.
 */

export const FEEDBACK_REPO = 'praveenvnktsh/minutes';

/** How long the issues preflight waits before giving up and assuming "open". */
const ISSUES_PREFLIGHT_TIMEOUT_MS = 4000;

export interface FeedbackDraft {
  title: string;
  description: string;
}

export interface FeedbackContext {
  version?: string;
  platform?: string;
}

function issueTitle(draft: FeedbackDraft): string {
  return draft.title.trim() || 'Feedback';
}

function buildIssueBody(description: string, context: FeedbackContext): string {
  const metadata: string[] = [];
  if (context.version) metadata.push(`**Version:** ${context.version}`);
  if (context.platform) metadata.push(`**Platform:** ${context.platform}`);

  const sections = [description.trim()];
  if (metadata.length > 0) {
    sections.push(`---\n\n${metadata.join('\n')}`);
  }
  return sections.filter(Boolean).join('\n\n');
}

/**
 * Builds the prefilled GitHub new-issue URL for a feedback draft.
 *
 * The title falls back to "Feedback" when the user leaves it blank so the
 * form never opens with an empty required field.
 */
export function buildFeedbackIssueUrl(
  draft: FeedbackDraft,
  context: FeedbackContext = {},
): string {
  const params = new URLSearchParams({
    title: issueTitle(draft),
    body: buildIssueBody(draft.description, context),
  });
  return `https://github.com/${FEEDBACK_REPO}/issues/new?${params.toString()}`;
}

/**
 * Renders a draft as plain text for the clipboard, with the same title and
 * metadata the issue body would carry. The dialog offers this when the issue
 * tracker is closed so the user still leaves with their report in hand.
 */
export function buildFeedbackClipboardText(
  draft: FeedbackDraft,
  context: FeedbackContext = {},
): string {
  return `${issueTitle(draft)}\n\n${buildIssueBody(draft.description, context)}`;
}

/**
 * Asks GitHub whether the feedback repository still accepts issues.
 *
 * Unknown means open: this resolves to `false` only when the API definitively
 * reports the repository with `has_issues: false`. A network error, a non-OK
 * or rate-limited response, a malformed body, a timeout or a runtime without
 * `fetch` all resolve to `true`, because an offline or flaky machine must
 * never be the reason a user cannot file feedback. It never throws.
 */
export async function feedbackIssuesAreOpen(): Promise<boolean> {
  if (typeof fetch !== 'function') return true;

  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), ISSUES_PREFLIGHT_TIMEOUT_MS);

  try {
    const response = await fetch(`https://api.github.com/repos/${FEEDBACK_REPO}`, {
      headers: { Accept: 'application/vnd.github+json' },
      signal: controller.signal,
    });
    if (!response.ok) return true;

    const repo: unknown = await response.json();
    if (typeof repo !== 'object' || repo === null) return true;
    return (repo as { has_issues?: unknown }).has_issues !== false;
  } catch {
    // Offline, aborted, rate limited or unparseable: assume issues are open.
    return true;
  } finally {
    clearTimeout(timeout);
  }
}

/**
 * Reads the version and platform from Tauri for the issue metadata. Outside
 * the desktop app (SSR, tests) both lookups fail and are simply omitted.
 */
export async function collectFeedbackContext(): Promise<FeedbackContext> {
  const context: FeedbackContext = {};

  try {
    const { getVersion } = await import('@tauri-apps/api/app');
    context.version = await getVersion();
  } catch {
    // Not running inside Tauri.
  }

  try {
    const { platform } = await import('@tauri-apps/plugin-os');
    context.platform = await platform();
  } catch {
    // Not running inside Tauri.
  }

  return context;
}
