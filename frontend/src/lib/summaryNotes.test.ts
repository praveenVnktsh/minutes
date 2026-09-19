import { afterAll, describe, expect, mock, test } from 'bun:test';

const calls: string[] = [];
const savedDocuments = new Map<string, unknown>();
const invoke = mock(async (command: string, args?: Record<string, unknown>) => {
  calls.push(command);
  if (command === 'save_meeting_live_notes') {
    if (args?.meetingId === 'failing-meeting') throw new Error('disk full');
    savedDocuments.set(args?.meetingId as string, args?.document);
    return;
  }
  if (command === 'get_meeting_live_notes') return savedDocuments.get(args?.meetingId as string) ?? null;
  throw new Error(`Unexpected command: ${command}`);
});
const originalCore = { ...await import('@tauri-apps/api/core') };
afterAll(() => mock.module('@tauri-apps/api/core', () => originalCore));
mock.module('@tauri-apps/api/core', () => ({ invoke }));

const { loadSummaryNotesContext } = await import('./autoSummary');
const { meetingNotesTarget, saveNotes } = await import('../services/notePersistenceService');

describe('summary note context', () => {
  test('flushes the current draft and converts authoritative editor blocks', async () => {
    const target = meetingNotesTarget('notes-meeting');
    saveNotes(target, {
      version: 1,
      meetingStartedAtMs: 1,
      updatedAt: '2026-09-18T00:00:00Z',
      notes: [],
      rawMarkdown: 'stale markdown',
      editorBlocks: [{ type: 'paragraph', content: [{ type: 'text', text: 'Current block', styles: {} }] }],
    });

    expect(await loadSummaryNotesContext('notes-meeting')).toBe('Current block');
    expect(calls.slice(0, 2)).toEqual(['save_meeting_live_notes', 'get_meeting_live_notes']);
  });

  test('rejects instead of generating from stale notes when flushing fails', async () => {
    const target = meetingNotesTarget('failing-meeting');
    saveNotes(target, {
      version: 1,
      meetingStartedAtMs: 1,
      updatedAt: '2026-09-18T00:00:00Z',
      notes: [],
      rawMarkdown: 'unsaved',
    });
    await expect(loadSummaryNotesContext('failing-meeting')).rejects.toThrow('disk full');
  });
});
