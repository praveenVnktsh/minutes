/**
 * Send-feedback helpers.
 *
 * `buildFeedbackIssueUrl` and `buildFeedbackClipboardText` are pure functions
 * so they can be tested without a browser or a Tauri runtime.
 * `collectFeedbackContext` reads the app version and platform from Tauri when
 * the dialog asks for them, and `feedbackIssuesAreOpen` checks with GitHub
 * whether the issue form is worth opening at all.
 */

import { invoke } from '@tauri-apps/api/core';

export const FEEDBACK_REPO = 'praveenvnktsh/minutes';

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
 * Asks whether the feedback repository still accepts issues.
 *
 * The request itself is made by the Rust command `feedback_issues_are_open`,
 * not from here: the window's `connect-src` policy allows no third-party host,
 * so a `fetch` to GitHub is blocked in a packaged build — and a preflight that
 * fails in production is worse than none, because it fails towards opening the
 * 404 it exists to prevent.
 *
 * Unknown means open. This resolves to `false` only when GitHub definitively
 * reports the repository with `has_issues: false`. A network error, a non-OK
 * or rate-limited response, a malformed body, a timeout or a runtime without
 * Tauri all resolve to `true`, because an offline or flaky machine must never
 * be the reason a user cannot file feedback. It never throws.
 */
export async function feedbackIssuesAreOpen(): Promise<boolean> {
  try {
    const open = await invoke<boolean>('feedback_issues_are_open', { repo: FEEDBACK_REPO });
    return open !== false;
  } catch {
    // Offline, rate limited, timed out, or not running inside Tauri.
    return true;
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
