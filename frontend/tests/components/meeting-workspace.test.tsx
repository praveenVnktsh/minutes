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

const { MeetingWorkspace } = await import('../../src/components/MeetingDetails/MeetingWorkspace');
const { SummaryPanel } = await import('../../src/components/MeetingDetails/SummaryPanel');

afterAll(() => mock.module('../../src/contexts/ShellContext', () => originalShell));

function renderWorkspace(summary: React.ReactNode) {
  return renderToStaticMarkup(
    <MeetingWorkspace
      title="Planning"
      createdAt="2026-09-18T10:00:00Z"
      notesMode="enhanced"
      onNotesModeChange={() => {}}
      canShowEnhanced
      hasEnhancedContent={false}
      summary={summary}
      rawNotes={<div>Raw notes</div>}
      transcript={<div>Transcript content</div>}
      assistant={<div>Assistant content</div>}
      showAssistant
      peopleCount={2}
    />,
  );
}

describe('MeetingWorkspace composition', () => {
  test('keeps first summary generation reachable through the enhanced surface', () => {
    compact = false;
    const html = renderWorkspace(
      <SummaryPanel
        meeting={{ id: 'meeting-1', title: 'Planning', created_at: '2026-09-18T10:00:00Z' }}
        meetingTitle="Planning"
        isSummaryDirty={false}
        summaryRef={{ current: null }}
        isSaving={false}
        onCopySummary={async () => {}}
        aiSummary={null}
        summaryStatus="idle"
        transcripts={[]}
        modelConfig={{ provider: 'ollama', model: 'gemma3:1b', whisperModel: 'base' }}
        onGenerateSummary={async () => {}}
        onStopGeneration={() => {}}
        customPrompt=""
        onSaveSummary={async () => {}}
        onSummaryChange={() => {}}
        onDirtyChange={() => {}}
        summaryError={null}
        onRegenerateSummary={async () => {}}
        getSummaryStatusMessage={() => ''}
        availableTemplates={[]}
        selectedTemplate="standard_meeting"
        onTemplateSelect={() => {}}
      />,
    );

    expect(html).toContain('Generate Summary');
    expect(html).toContain('aria-pressed="true"');
    expect(html).not.toContain('Re-enhance notes');
  });

  test('initializes compact workspaces without opening the transcript dock', () => {
    compact = true;
    const html = renderWorkspace(<div>Enhanced notes</div>);
    expect(html).toContain('aria-pressed="false"');
    expect(html).not.toContain('Transcript content');
  });

  test('shows retry instead of Generate while the initial summary read is unknown', () => {
    const html = renderToStaticMarkup(
      <SummaryPanel
        meeting={{ id: 'meeting-1', title: 'Planning', created_at: '2026-09-18T10:00:00Z' }}
        meetingTitle="Planning"
        isSummaryDirty={false}
        summaryRef={{ current: null }}
        isSaving={false}
        onCopySummary={async () => {}}
        aiSummary={null}
        summaryStatus="idle"
        transcripts={[]}
        modelConfig={{ provider: 'ollama', model: 'gemma3:1b', whisperModel: 'base' }}
        onGenerateSummary={async () => {}}
        onStopGeneration={() => {}}
        customPrompt=""
        onSaveSummary={async () => {}}
        onSummaryChange={() => {}}
        onDirtyChange={() => {}}
        summaryError={null}
        summaryReadError="database busy"
        onRetrySummaryRead={() => {}}
        onRegenerateSummary={async () => {}}
        getSummaryStatusMessage={() => ''}
        availableTemplates={[]}
        selectedTemplate="standard_meeting"
        onTemplateSelect={() => {}}
      />,
    );
    expect(html).toContain('Could not load enhanced notes');
    expect(html).toContain('Retry');
    expect(html).not.toContain('Generate Summary');
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
          notesMode="raw"
          onNotesModeChange={() => {}}
          canShowEnhanced
          summary={<div />}
          rawNotes={<div />}
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
