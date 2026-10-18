import { afterAll, afterEach, beforeEach, describe, expect, mock, test } from 'bun:test';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import type { SummaryProcessResponse } from '../../src/types';

const originalCore = { ...await import('@tauri-apps/api/core') };
const originalConfig = { ...await import('../../src/contexts/ConfigContext') };
const originalNavigation = { ...await import('next/navigation') };
const originalAnalytics = { ...await import('../../src/lib/analytics') };
const originalToast = { ...await import('sonner') };
const originalRecordingState = { ...await import('../../src/contexts/RecordingStateContext') };
const originalPreferences = { ...await import('../../src/lib/summary-language-preferences') };

let nativeSummary: SummaryProcessResponse;
let startPromise: Promise<{ process_id: string }> | null = null;
let renamePromise: Promise<unknown> | null = null;
const catalog = [
  { id: 'meeting-a', title: 'Meeting A', created_at: '2026-09-19', pinned: false },
  { id: 'meeting-b', title: 'Meeting B', created_at: '2026-09-19', pinned: false },
];
const invocations: Array<[string, Record<string, unknown> | undefined]> = [];
const invoke = mock(async (command: string, args?: Record<string, unknown>): Promise<unknown> => {
  invocations.push([command, args]);
  if (command === 'api_get_meetings') return catalog;
  if (command === 'api_get_summary') return nativeSummary;
  if (command === 'api_get_meeting_transcripts') return {
    transcripts: [{ id: 't1', text: 'Transcript', timestamp: '00:00' }], total_count: 1, has_more: false,
  };
  if (command === 'api_process_transcript') return startPromise ?? { process_id: 'attempt-new' };
  if (command === 'api_cancel_summary') return { cancelled: true };
  if (command === 'api_save_meeting_summary') return { message: 'saved' };
  if (command === 'api_save_meeting_title') return renamePromise ?? true;
  if (command === 'api_set_meeting_pinned') return true;
  if (command === 'get_meeting_folder_path') return null;
  return undefined;
});
mock.module('@tauri-apps/api/core', () => ({ ...originalCore, invoke }));
mock.module('../../src/contexts/ConfigContext', () => ({
  useConfig: () => ({
    modelConfig: { provider: 'ollama', model: 'test', whisperModel: 'base' },
    isModelConfigLoading: false,
    isModelConfigSaving: false,
    modelConfigSaveError: null,
  }),
}));
const push = mock(() => {});
mock.module('next/navigation', () => ({ useRouter: () => ({ push }) }));
const analytics = {
  trackPageView() {}, trackButtonClick() {}, trackCustomPromptUsed: async () => {},
  trackSummaryGenerationStarted: async () => {}, trackSummaryGenerationCompleted: async () => {},
  trackBackendConnection() {},
};
mock.module('../../src/lib/analytics', () => ({ ...analytics, default: analytics }));
mock.module('sonner', () => ({ toast: { info() {}, error() {}, success() {}, warning() {} } }));
mock.module('../../src/contexts/RecordingStateContext', () => ({
  useRecordingState: () => ({ isRecording: false }),
}));
mock.module('../../src/lib/summary-language-preferences', () => ({
  readMeetingSummaryLanguage: async () => ({ language: 'en', storage: 'metadata' }),
  readCachedDetectedSummaryLanguage: async () => 'en',
  detectAndCacheSummaryLanguage: async () => ({ language: 'en' }),
}));
mock.module('../../src/lib/autoSummary', () => ({
  validateSummaryModel: async () => {}, loadSummaryNotesContext: async () => '',
}));
mock.module('../../src/hooks/meeting-details/useTemplates', () => ({ useTemplates: () => ({
  availableTemplates: [], selectedTemplate: 'standard_meeting', handleTemplateSelection() {},
}) }));
mock.module('../../src/hooks/meeting-details/useCopyOperations', () => ({ useCopyOperations: () => ({
  handleCopyTranscript: async () => {}, handleCopyNotes: async () => {}, handleExportMarkdown: async () => {},
}) }));
mock.module('../../src/hooks/meeting-details/useMeetingOperations', () => ({ useMeetingOperations: () => ({
  handleOpenMeetingFolder: async () => {},
}) }));
mock.module('framer-motion', () => ({ motion: { div: ({ children }: { children?: unknown }) => children } }));
mock.module('../../src/components/ui/popover', () => ({
  Popover: ({ children }: { children?: unknown }) => children,
  PopoverContent: ({ children }: { children?: unknown }) => children,
  PopoverTrigger: ({ children }: { children?: unknown }) => children,
}));


let workspaceProps: any;
let notesProps: any;
mock.module('../../src/components/MeetingDetails/MeetingWorkspace', () => ({
  MeetingWorkspace: (props: any) => {
    workspaceProps = props;
    return <>{props.statusBanner}{props.notesNotice}{props.notes}{props.toolbarActions}</>;
  },
}));
mock.module('../../src/components/MeetingDetails/MeetingNotesEditor', () => ({
  MeetingNotesEditor: (props: any) => {
    notesProps = props;
    return <output>{props.legacyNotes}</output>;
  },
}));
mock.module('../../src/components/MeetingDetails/TranscriptPanel', () => ({ TranscriptPanel: () => null }));
mock.module('../../src/components/MeetingDetails/MeetingAssistantPanel', () => ({ MeetingAssistantPanel: () => null }));
mock.module('../../src/components/MeetingDetails/LiveNotes', () => ({ LiveNotes: () => null, LiveNotesNotice: () => null }));
mock.module('../../src/components/MeetingDetails/LiveTranscriptPanel', () => ({ LiveTranscriptPanel: () => null }));
mock.module('../../src/components/MeetingDetails/FloatingRecordingControls', () => ({ FloatingRecordingControls: () => null }));
mock.module('../../src/components/LiveNotesPad', () => ({ LiveNotesPad: () => null }));


const { SidebarProvider, useSidebar } = await import('../../src/components/Sidebar/SidebarProvider');
const { meetingActivityStore } = await import('../../src/contexts/MeetingActivityContext');
const { default: PageContent } = await import('../../src/app/meeting-details/page-content');

afterAll(() => {
  mock.module('@tauri-apps/api/core', () => originalCore);
  mock.module('../../src/contexts/ConfigContext', () => originalConfig);
  mock.module('next/navigation', () => originalNavigation);
  mock.module('../../src/lib/analytics', () => originalAnalytics);
  mock.module('sonner', () => originalToast);
  mock.module('../../src/contexts/RecordingStateContext', () => originalRecordingState);
  mock.module('../../src/lib/summary-language-preferences', () => originalPreferences);
});

const response = (markdown: string): SummaryProcessResponse => ({
  meeting_id: 'meeting-a', status: 'completed', start: '2026-09-19T10:00:00.000000000Z', end: null,
  data: { markdown }, error: null, meetingName: 'Meeting A',
});
const meeting = {
  id: 'meeting-a', title: 'Meeting A', created_at: '2026-09-19T09:00:00Z', transcripts: [{ id: 't1', text: 'Transcript', timestamp: '00:00' }],
};
let renderer: ReactTestRenderer | undefined;
let eventTarget: EventTarget;
let sidebar: ReturnType<typeof useSidebar>;
function SidebarProbe() {
  sidebar = useSidebar();
  return null;
}

beforeEach(() => {
  nativeSummary = response('Fresh saved B');
  startPromise = null;
  renamePromise = null;
  invocations.length = 0;
  workspaceProps = null;
  eventTarget = new EventTarget();
  Object.defineProperty(globalThis, 'window', {
    configurable: true,
    value: Object.assign(eventTarget, { innerWidth: 1200 }),
  });
});
afterEach(async () => {
  if (renderer) await act(async () => renderer!.unmount());
  renderer = undefined;
  meetingActivityStore.stopSummaryPolling('meeting-a');
  meetingActivityStore.dismissSummary('meeting-a');
});

async function show(onRefetchTranscripts = mock(async () => {}), initialSummaryError?: string) {
  await act(async () => {
    renderer = create(
      <SidebarProvider>
        <SidebarProbe />
        <PageContent
          meeting={meeting}
          summaryData={{ markdown: 'Fresh saved B' }}
          initialSummary={nativeSummary}
          initialSummaryError={initialSummaryError}
          onRefetchTranscripts={onRefetchTranscripts}
        />
      </SidebarProvider>,
    );
  });
  return onRefetchTranscripts;
}


/** A button in the notes menu (or banner), by its text. */
function button(label: string) {
  return renderer!.root.findAll((node) => node.type === 'button'
    && node.children.some((child) => typeof child === 'string' && child.includes(label)))[0];
}
const rendered = () => JSON.stringify(renderer!.toJSON());

describe('PageContent owner repair composition', () => {
  test('the fresh route document is the old AI notes shown below the notes', async () => {
    meetingActivityStore.hydrateSummary(response('Retained generated A'));
    await show();
    expect(notesProps.meetingId).toBe('meeting-a');
    expect(notesProps.legacyNotes).toBe('Fresh saved B');
  });

  test('no Raw/Enhanced split: the workspace gets one notes document', async () => {
    await show();
    expect(workspaceProps.notes).toBeDefined();
    expect(workspaceProps.summary).toBeUndefined();
    expect(workspaceProps.notesMode).toBeUndefined();
    expect(button('Copy notes')).toBeDefined();
    expect(button('Export')).toBeDefined();
  });

  test('a persistence-complete event refreshes only the current meeting', async () => {
    const refetch = await show();
    await act(async () => { eventTarget.dispatchEvent(new CustomEvent('meetily:recording-finalized', { detail: { meetingId: 'other' } })); });
    expect(refetch).not.toHaveBeenCalled();
    await act(async () => { eventTarget.dispatchEvent(new CustomEvent('meetily:recording-finalized', { detail: { meetingId: 'meeting-a' } })); });
    expect(refetch).toHaveBeenCalledTimes(1);
  });

  test('offers a retry, and no rewrite, while the initial summary read is unavailable', async () => {
    await show(undefined, 'database busy');
    expect(rendered()).toContain('Could not load AI notes');
    expect(button('Rewrite AI notes')).toBeUndefined();
    expect(button('Write AI notes')).toBeUndefined();
  });

  test('pre-start hydration cannot replace a rewrite with completed or hide Stop', async () => {
    let resolveStart!: (value: { process_id: string }) => void;
    startPromise = new Promise((resolve) => { resolveStart = resolve; });
    await show();
    await act(async () => {
      button('Rewrite AI notes').props.onClick();
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(rendered()).toContain('Finishing notes');

    await act(async () => { button('Stop').props.onClick(); });
    await act(async () => resolveStart({ process_id: '2026-09-19T11:00:00.000000000Z' }));
    await act(async () => { await new Promise((resolve) => setTimeout(resolve, 0)); });
    expect(invocations.some(([command]) => command === 'api_cancel_summary')).toBe(true);
  });

  test('adopts an externally started c6 attempt and its fresh terminal document while mounted', async () => {
    let resolveStart!: (value: { process_id: string }) => void;
    startPromise = new Promise((resolve) => { resolveStart = resolve; });
    await show();
    const starting = meetingActivityStore.startSummary({
      meetingId: meeting.id,
      text: 'Transcript',
      model: 'ollama',
      modelName: 'test',
      chunkSize: 40000,
      overlap: 1000,
      customPrompt: '',
      templateId: 'standard_meeting',
      summaryLanguage: 'en',
      replaceExisting: true,
    });
    await act(async () => { await Promise.resolve(); });
    expect(rendered()).toContain('Finishing notes');

    nativeSummary = response('Externally generated C');
    nativeSummary.start = '2026-09-19T11:00:00.000000000Z';
    await act(async () => {
      resolveStart({ process_id: nativeSummary.start! });
      await starting;
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(rendered()).not.toContain('Finishing notes');
    expect(notesProps.legacyNotes).toBe('Externally generated C');
  });

  test('delayed title completion cannot restore an obsolete catalog or selection', async () => {
    let resolveRename!: (value: unknown) => void;
    renamePromise = new Promise((resolve) => { resolveRename = resolve; });
    await show();
    const completeRename = workspaceProps.onTitleChange('Renamed A');
    await act(async () => {
      sidebar.setCurrentMeeting({ id: 'meeting-b', title: 'Meeting B' });
      await sidebar.setMeetingPinned('meeting-b', true);
      sidebar.setMeetings([...sidebar.meetings, { id: 'meeting-c', title: 'Meeting C', created_at: '2026-09-19' }]);
    });
    await act(async () => resolveRename(true));
    await act(async () => completeRename);

    expect(sidebar.currentMeeting?.id).toBe('meeting-b');
    expect(sidebar.meetings.find((item) => item.id === 'meeting-b')?.pinned).toBe(true);
    expect(sidebar.meetings.some((item) => item.id === 'meeting-c')).toBe(true);
  });
});
