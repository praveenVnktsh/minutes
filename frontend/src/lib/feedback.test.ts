import { describe, expect, test } from 'bun:test';
import { FEEDBACK_REPO, buildFeedbackIssueUrl } from './feedback';

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
