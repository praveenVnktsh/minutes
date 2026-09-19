import { invoke } from '@tauri-apps/api/core';
import type { ModelConfig } from '@/services/configService';
import type { Transcript } from '@/types';
import {
  detectAndCacheSummaryLanguage,
  readCachedDetectedSummaryLanguage,
  readMeetingSummaryLanguage,
} from '@/lib/summary-language-preferences';
import { fetchCompleteTranscripts, originalNotesMarkdown } from '@/lib/meetingExport';
import {
  flushNotes,
  loadNotes,
  meetingNotesTarget,
  notePersistenceService,
  type NoteDocumentTarget,
} from '@/services/notePersistenceService';
import { meetingActivityStore } from '@/contexts/MeetingActivityContext';

const PENDING_DEFERRED_SUMMARIES_KEY = 'meetily:pending-deferred-auto-summaries';

function readPendingMeetingIds(): Set<string> {
  try {
    const stored = JSON.parse(localStorage.getItem(PENDING_DEFERRED_SUMMARIES_KEY) || '[]');
    return new Set(Array.isArray(stored) ? stored.filter((id): id is string => typeof id === 'string') : []);
  } catch {
    return new Set();
  }
}

function writePendingMeetingIds(ids: Set<string>) {
  localStorage.setItem(PENDING_DEFERRED_SUMMARIES_KEY, JSON.stringify([...ids]));
}

export function markDeferredMeetingForAutoSummary(meetingId: string) {
  const ids = readPendingMeetingIds();
  ids.add(meetingId);
  writePendingMeetingIds(ids);
}

export function consumeDeferredMeetingForAutoSummary(meetingId: string): boolean {
  const ids = readPendingMeetingIds();
  if (!ids.delete(meetingId)) return false;
  writePendingMeetingIds(ids);
  return true;
}

const claimedAutoSummaryJobs = new Set<string>();

function autoSummaryJobKey(
  taskId: string,
  modelConfig: Pick<ModelConfig, 'provider' | 'model'>,
): string {
  return `${taskId}\u0000${modelConfig.provider}\u0000${modelConfig.model}`;
}

export function claimAutoSummaryJob(
  taskId: string,
  modelConfig: Pick<ModelConfig, 'provider' | 'model'>,
): boolean {
  const key = autoSummaryJobKey(taskId, modelConfig);
  if (claimedAutoSummaryJobs.has(key)) return false;
  claimedAutoSummaryJobs.add(key);
  return true;
}

export function releaseAutoSummaryJob(
  taskId: string,
  modelConfig: Pick<ModelConfig, 'provider' | 'model'>,
): void {
  claimedAutoSummaryJobs.delete(autoSummaryJobKey(taskId, modelConfig));
}

export async function loadSummaryNotesContext(
  meetingId: string,
  target: NoteDocumentTarget = meetingNotesTarget(meetingId),
): Promise<string> {
  await flushNotes(target);
  let snapshot = notePersistenceService.getSnapshot(target);
  if (snapshot.loadState === 'idle' || snapshot.loadState === 'loading') {
    snapshot = await loadNotes(target);
  }
  if (snapshot.loadState === 'error') {
    throw snapshot.loadError ?? new Error('Could not load the current meeting notes.');
  }
  return originalNotesMarkdown(snapshot.document);
}

async function fetchAllTranscripts(meetingId: string): Promise<Transcript[]> {
  return fetchCompleteTranscripts(meetingId, (args) => invoke('api_get_meeting_transcripts', args));
}

async function resolveSummaryLanguage(meetingId: string, transcriptTexts: string[]) {
  const meetingPreference = await readMeetingSummaryLanguage(meetingId).catch(() => null);
  if (meetingPreference?.language) return meetingPreference.language;

  const cachedLanguage = await readCachedDetectedSummaryLanguage(meetingId).catch(() => null);
  if (cachedLanguage) return cachedLanguage;

  const detected = await detectAndCacheSummaryLanguage(meetingId, transcriptTexts).catch(() => null);
  return detected?.language ?? null;
}

export async function validateSummaryModel(modelConfig: ModelConfig): Promise<void> {
  if (modelConfig.provider === 'ollama') {
    const models = await invoke<unknown[]>('get_ollama_models', {
      endpoint: modelConfig.ollamaEndpoint || null,
    });
    if (models.length === 0) {
      throw new Error('No Ollama models found. Please download gemma3:1b from Model Settings.');
    }
  }
  if (modelConfig.provider === 'builtin-ai') {
    if (!modelConfig.model) throw new Error('No built-in AI model selected. Please select a model in settings.');
    const ready = await invoke<boolean>('builtin_ai_is_model_ready', {
      modelName: modelConfig.model,
      refresh: true,
    });
    if (!ready) throw new Error('Built-in AI model is not ready. Please check model settings.');
  }
}

export async function generateAutomaticSummary(meetingId: string, modelConfig: ModelConfig) {
  const result = await meetingActivityStore.prepareAndStartSummary(meetingId, async () => {
    const transcripts = await fetchAllTranscripts(meetingId);
    const transcriptTexts = transcripts.map(({ text }) => text.trim()).filter(Boolean);
    if (transcriptTexts.length === 0) throw new Error('No transcripts available for summary.');
    const notesContext = await loadSummaryNotesContext(meetingId);
    await validateSummaryModel(modelConfig);
    const summaryLanguage = await resolveSummaryLanguage(meetingId, transcriptTexts);
    return {
      text: transcriptTexts.join('\n'),
      model: modelConfig.provider,
      modelName: modelConfig.model,
      meetingId,
      chunkSize: 40000,
      overlap: 1000,
      customPrompt: notesContext,
      templateId: 'standard_meeting',
      summaryLanguage,
      replaceExisting: false,
    };
  });

  return {
    started: result.started,
    reason: result.started ? 'started' as const : 'summary-exists' as const,
  };
}
