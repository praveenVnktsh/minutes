import { afterAll, describe, expect, mock, test } from 'bun:test';

// Restore the real module afterwards so a mocked `invoke` cannot leak into a
// later file.
const originalCore = { ...(await import('@tauri-apps/api/core')) };
afterAll(() => {
  mock.module('@tauri-apps/api/core', () => originalCore);
});

let invokeResult: () => Promise<unknown> = async () => true;
const invokeCalls: Array<[string, unknown]> = [];

mock.module('@tauri-apps/api/core', () => ({
  invoke: async (command: string, args?: unknown) => {
    invokeCalls.push([command, args]);
    return await invokeResult();
  },
}));

// The feedback module must load after the Tauri invoke mock is registered.
const {
  FEEDBACK_REPO,
  buildFeedbackClipboardText,
  buildFeedbackIssueUrl,
  feedbackIssuesAreOpen,
} = await import('./feedback');

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
  test('asks the Rust command about the repository the issue form points at', async () => {
    invokeCalls.length = 0;
    invokeResult = async () => true;

    await feedbackIssuesAreOpen();

    expect(invokeCalls).toEqual([['feedback_issues_are_open', { repo: FEEDBACK_REPO }]]);
  });

  test('is false when GitHub reports the tracker is closed', async () => {
    invokeResult = async () => false;

    expect(await feedbackIssuesAreOpen()).toBe(false);
  });

  test('is true when GitHub reports the tracker is open', async () => {
    invokeResult = async () => true;

    expect(await feedbackIssuesAreOpen()).toBe(true);
  });

  test('is true when the command fails', async () => {
    invokeResult = async () => {
      throw new Error('Could not reach GitHub: offline');
    };

    expect(await feedbackIssuesAreOpen()).toBe(true);
  });

  test('is true when there is no Tauri runtime to answer', async () => {
    invokeResult = async () => undefined;

    expect(await feedbackIssuesAreOpen()).toBe(true);
  });
});
