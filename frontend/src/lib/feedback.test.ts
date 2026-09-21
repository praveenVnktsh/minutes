import { afterEach, describe, expect, test } from 'bun:test';
import {
  FEEDBACK_REPO,
  buildFeedbackClipboardText,
  buildFeedbackIssueUrl,
  feedbackIssuesAreOpen,
} from './feedback';

const originalFetch = globalThis.fetch;

afterEach(() => {
  globalThis.fetch = originalFetch;
});

function stubFetch(handler: () => Promise<unknown>): void {
  globalThis.fetch = (async () => await handler()) as unknown as typeof fetch;
}

function jsonResponse(body: unknown, ok = true): unknown {
  return { ok, json: async () => body };
}

describe('buildFeedbackIssueUrl', () => {
  test('targets the project issue form with the draft prefilled', () => {
    const url = new URL(
      buildFeedbackIssueUrl({ title: 'Export fails', description: 'Steps to reproduce' }),
    );

    expect(url.origin).toBe('https://github.com');
    expect(url.pathname).toBe(`/${FEEDBACK_REPO}/issues/new`);
    expect(url.searchParams.get('title')).toBe('Export fails');
    expect(url.searchParams.get('body')).toContain('Steps to reproduce');
  });

  test('falls back to a placeholder title', () => {
    const url = new URL(buildFeedbackIssueUrl({ title: '   ', description: 'hello' }));

    expect(url.searchParams.get('title')).toBe('Feedback');
  });

  test('appends version and platform to the body', () => {
    const url = new URL(
      buildFeedbackIssueUrl(
        { title: 'Crash on launch', description: 'It crashed' },
        { version: '1.5.3', platform: 'linux' },
      ),
    );

    const body = url.searchParams.get('body') ?? '';
    expect(body).toContain('It crashed');
    expect(body).toContain('**Version:** 1.5.3');
    expect(body).toContain('**Platform:** linux');
  });

  test('omits metadata when no context is available', () => {
    const url = new URL(buildFeedbackIssueUrl({ title: 'Idea', description: 'Nice to have' }));

    expect(url.searchParams.get('body')).toBe('Nice to have');
  });
});

describe('buildFeedbackClipboardText', () => {
  test('carries the title, description and metadata', () => {
    const text = buildFeedbackClipboardText(
      { title: 'Crash on launch', description: 'It crashed' },
      { version: '1.5.3', platform: 'linux' },
    );

    expect(text).toContain('Crash on launch');
    expect(text).toContain('It crashed');
    expect(text).toContain('**Version:** 1.5.3');
    expect(text).toContain('**Platform:** linux');
  });

  test('falls back to a placeholder title', () => {
    expect(buildFeedbackClipboardText({ title: '  ', description: 'hello' })).toBe(
      'Feedback\n\nhello',
    );
  });
});

describe('feedbackIssuesAreOpen', () => {
  test('is false when GitHub reports the tracker is closed', async () => {
    stubFetch(async () => jsonResponse({ has_issues: false }));

    expect(await feedbackIssuesAreOpen()).toBe(false);
  });

  test('is true when GitHub reports the tracker is open', async () => {
    stubFetch(async () => jsonResponse({ has_issues: true }));

    expect(await feedbackIssuesAreOpen()).toBe(true);
  });

  test('queries the repository the issue form points at', async () => {
    const requested: string[] = [];
    globalThis.fetch = (async (input: string) => {
      requested.push(input);
      return jsonResponse({ has_issues: true });
    }) as unknown as typeof fetch;

    await feedbackIssuesAreOpen();

    expect(requested).toEqual([`https://api.github.com/repos/${FEEDBACK_REPO}`]);
  });

  test('is true when the request fails', async () => {
    stubFetch(async () => {
      throw new Error('offline');
    });

    expect(await feedbackIssuesAreOpen()).toBe(true);
  });

  test('is true when the response is not ok', async () => {
    stubFetch(async () => jsonResponse({ message: 'rate limit exceeded' }, false));

    expect(await feedbackIssuesAreOpen()).toBe(true);
  });

  test('is true when the body is malformed', async () => {
    stubFetch(async () => ({
      ok: true,
      json: async () => {
        throw new SyntaxError('Unexpected token');
      },
    }));

    expect(await feedbackIssuesAreOpen()).toBe(true);
  });
});
