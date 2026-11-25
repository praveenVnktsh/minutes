import { afterAll, describe, expect, mock, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { act, create } from 'react-test-renderer';

const originalShell = { ...await import('../../src/contexts/ShellContext') };
let compact = false;

mock.module('../../src/contexts/ShellContext', () => ({
  ...originalShell,
  SIDEBAR_WIDTH: 280,
  SIDEBAR_COLLAPSED_WIDTH: 72,
  useShell: () => ({ compact, collapsed: false }),
}));

// Radix's Switch needs a DOM; a plain stand-in keeps its props.
const originalSwitch = { ...await import('../../src/components/ui/switch') };
mock.module('../../src/components/ui/switch', () => ({
  Switch: (props: { checked: boolean; onCheckedChange: (value: boolean) => void; 'aria-label'?: string }) => (
    <button type="button" role="switch" aria-checked={props.checked} aria-label={props['aria-label']} />
  ),
}));

const { MeetingWorkspace } = await import('../../src/components/MeetingDetails/MeetingWorkspace');
const { readShowAiNotes, resetShowAiNotesCache, SHOW_AI_NOTES_KEY } = await import('../../src/lib/aiNotes');

afterAll(() => {
  mock.module('../../src/contexts/ShellContext', () => originalShell);
  mock.module('../../src/components/ui/switch', () => originalSwitch);
});

function renderWorkspace(notes: React.ReactNode) {
  return renderToStaticMarkup(
    <MeetingWorkspace
      title="Planning"
      createdAt="2026-09-18T10:00:00Z"
      notes={notes}
      transcript={<div>Transcript content</div>}
      assistant={<div>Assistant content</div>}
      showAssistant
      peopleCount={2}
    />,
  );
}

describe('MeetingWorkspace composition', () => {
  test('one notes document: no Raw/Enhanced switch, no re-enhance, a Show AI notes switch', () => {
    compact = false;
    const html = renderWorkspace(<div>Notes document</div>);
    expect(html).toContain('Notes document');
    expect(html).toContain('Show AI notes');
    expect(html).not.toContain('Enhanced');
    expect(html).not.toContain('Raw');
    expect(html).not.toContain('Re-enhance');
  });

  test('keeps the transcript dock open in compact (non-maximized) windows', () => {
    compact = true;
    const html = renderWorkspace(<div>Notes</div>);
    expect(html).toContain('title="Toggle transcript" aria-pressed="true"');
    expect(html).toContain('Transcript content');
  });

  test('the Show AI notes switch is on by default and remembers being turned off', async () => {
    compact = false;
    const values = new Map<string, string>();
    Object.defineProperty(globalThis, 'localStorage', {
      configurable: true,
      value: { getItem: (key: string) => values.get(key) ?? null, setItem: (key: string, value: string) => { values.set(key, value); } },
    });
    Object.defineProperty(globalThis, 'window', {
      configurable: true,
      value: { innerWidth: 1200, addEventListener: () => {}, removeEventListener: () => {} },
    });
    resetShowAiNotesCache();

    let renderer!: ReturnType<typeof create>;
    await act(async () => {
      renderer = create(
        <MeetingWorkspace
          title="Planning"
          createdAt="2026-09-18T10:00:00Z"
          notes={<div />}
          transcript={<div />}
          assistant={<div />}
          showAssistant
          peopleCount={0}
        />,
        { createNodeMock: () => ({ getBoundingClientRect: () => ({ width: 920, height: 600 }) }) },
      );
    });
    const toggle = () => renderer.root.findAll((node) => node.props['aria-label'] === 'Show AI notes' && 'onCheckedChange' in node.props)[0];
    expect(toggle().props.checked).toBe(true);
    await act(async () => toggle().props.onCheckedChange(false));
    expect(toggle().props.checked).toBe(false);
    expect(values.get(SHOW_AI_NOTES_KEY)).toBe('false');
    resetShowAiNotesCache();
    expect(readShowAiNotes()).toBe(false);
    await act(async () => toggle().props.onCheckedChange(true));
    await act(async () => renderer.unmount());
  });


  test('keeps a failed title draft, cancels on Escape, and resizes with the keyboard', async () => {
    compact = false;
    const rename = mock(async () => { throw new Error('database busy'); });
    Object.defineProperty(globalThis, 'localStorage', {
      configurable: true,
      value: { getItem: () => null, setItem: () => {} },
    });
    Object.defineProperty(globalThis, 'window', {
      configurable: true,
      value: { innerWidth: 1200, addEventListener: () => {}, removeEventListener: () => {} },
    });

    let renderer!: ReturnType<typeof create>;
    await act(async () => {
      renderer = create(
        <MeetingWorkspace
          title="Planning"
          createdAt="2026-09-18T10:00:00Z"
          notes={<div />}
          transcript={<div />}
          assistant={<div />}
          showAssistant
          peopleCount={0}
          onTitleChange={rename}
        />,
        { createNodeMock: () => ({ getBoundingClientRect: () => ({ width: 920, height: 600 }) }) },
      );
    });

    let title = renderer.root.findByProps({ 'aria-label': 'Meeting title' });
    await act(async () => title.props.onChange({ target: { value: 'Draft title' } }));
    title = renderer.root.findByProps({ 'aria-label': 'Meeting title' });
    await act(async () => title.props.onBlur());
    expect(renderer.root.findByProps({ 'aria-label': 'Meeting title' }).props.value).toBe('Draft title');
    expect(renderer.root.findByProps({ role: 'alert' }).children.join('')).toContain('database busy');

    title = renderer.root.findByProps({ 'aria-label': 'Meeting title' });
    await act(async () => title.props.onChange({ target: { value: 'Discard me' } }));
    title = renderer.root.findByProps({ 'aria-label': 'Meeting title' });
    await act(async () => title.props.onKeyDown({ key: 'Escape', preventDefault: () => {}, target: { blur: () => {} } }));
    title = renderer.root.findByProps({ 'aria-label': 'Meeting title' });
    await act(async () => title.props.onBlur());
    expect(title.props.value).toBe('Planning');
    expect(rename).toHaveBeenCalledTimes(1);

    const separator = renderer.root.findAllByProps({ role: 'separator' })[0];
    const previous = separator.props['aria-valuenow'];
    await act(async () => separator.props.onKeyDown({ key: 'ArrowLeft', preventDefault: () => {}, defaultPrevented: false }));
    expect(renderer.root.findAllByProps({ role: 'separator' })[0].props['aria-valuenow']).toBe(previous - 10);
    await act(async () => renderer.unmount());
  });
});
