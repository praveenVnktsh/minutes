/**
 * Send-feedback helpers.
 *
 * `buildFeedbackIssueUrl` is a pure function so the issue URL can be tested
 * without a browser or a Tauri runtime. `collectFeedbackContext` reads the
 * app version and platform from Tauri when the dialog asks for them.
 */

export const FEEDBACK_REPO = 'praveenvnktsh/minutes';

export interface FeedbackDraft {
  title: string;
  description: string;
}

export interface FeedbackContext {
  version?: string;
  platform?: string;
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
    title: draft.title.trim() || 'Feedback',
    body: buildIssueBody(draft.description, context),
  });
  return `https://github.com/${FEEDBACK_REPO}/issues/new?${params.toString()}`;
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
