import { invoke } from '@tauri-apps/api/core';
import type { SaveFeedbackState } from '@/components/ui/status-feedback';
import {
  LIVE_NOTES_FALLBACK_FOLDER_KEY,
  LIVE_NOTES_FALLBACK_KEY,
  type LiveNotesDocument,
} from '@/lib/liveNotes';

export type NoteDocumentTarget =
  | { kind: 'meeting'; meetingId: string }
  | { kind: 'live'; sessionId: string; folderPath: string };

export type NoteLoadState = 'idle' | 'loading' | 'ready' | 'error';

export interface NotePersistenceSnapshot {
  target: NoteDocumentTarget;
  document: LiveNotesDocument | null;
  loadState: NoteLoadState;
  saveState: SaveFeedbackState;
  revision: number;
  acknowledgedRevision: number;
  loadError: unknown | null;
  saveError: unknown | null;
}

interface DraftEnvelope {
  revision: number;
  acknowledgedRevision: number;
  document: LiveNotesDocument;
}

interface NoteEntry extends NotePersistenceSnapshot {
  listeners: Set<() => void>;
  loadedFallback: boolean;
  saveTimer: ReturnType<typeof setTimeout> | null;
  writePromise: Promise<void> | null;
  loadRequest: number;
}

export interface NotePersistenceDependencies {
  storage?: Pick<Storage, 'getItem' | 'setItem' | 'removeItem'>;
  load?: (target: NoteDocumentTarget) => Promise<LiveNotesDocument | null>;
  save?: (target: NoteDocumentTarget, document: LiveNotesDocument) => Promise<void>;
  debounceMs?: number;
}

export interface LoadNotesOptions {
  createEmpty?: () => LiveNotesDocument;
}

export interface FlushNotesScope {
  meetingId?: string;
  sessionId?: string;
}

const DRAFT_KEY_PREFIX = 'meetily.notes.draft.v2';

export function meetingNotesTarget(meetingId: string): NoteDocumentTarget {
  return { kind: 'meeting', meetingId };
}

export function liveNotesTarget(folderPath: string): NoteDocumentTarget {
  return { kind: 'live', sessionId: folderPath, folderPath };
}

export function noteDocumentId(target: NoteDocumentTarget): string {
  return target.kind === 'meeting'
    ? `meeting:${target.meetingId}`
    : `live:${target.sessionId}`;
}

function draftKey(target: NoteDocumentTarget): string {
  return `${DRAFT_KEY_PREFIX}.${encodeURIComponent(noteDocumentId(target))}`;
}

function browserStorage(): Pick<Storage, 'getItem' | 'setItem' | 'removeItem'> | undefined {
  return typeof localStorage === 'undefined' ? undefined : localStorage;
}

async function loadFromTauri(target: NoteDocumentTarget): Promise<LiveNotesDocument | null> {
  return target.kind === 'meeting'
    ? invoke<LiveNotesDocument | null>('get_meeting_live_notes', { meetingId: target.meetingId })
    : invoke<LiveNotesDocument | null>('load_live_notes', { folderPath: target.folderPath });
}

async function saveToTauri(target: NoteDocumentTarget, document: LiveNotesDocument): Promise<void> {
  if (target.kind === 'meeting') {
    await invoke('save_meeting_live_notes', { meetingId: target.meetingId, document });
    return;
  }
  await invoke('save_live_notes', { folderPath: target.folderPath, document });
}

export class NotePersistenceService {
  private readonly entries = new Map<string, NoteEntry>();
  private readonly storage?: Pick<Storage, 'getItem' | 'setItem' | 'removeItem'>;
  private readonly loadDocument: (target: NoteDocumentTarget) => Promise<LiveNotesDocument | null>;
  private readonly saveDocument: (target: NoteDocumentTarget, document: LiveNotesDocument) => Promise<void>;
  private readonly debounceMs: number;

  constructor(dependencies: NotePersistenceDependencies = {}) {
    this.storage = dependencies.storage ?? browserStorage();
    this.loadDocument = dependencies.load ?? loadFromTauri;
    this.saveDocument = dependencies.save ?? saveToTauri;
    this.debounceMs = dependencies.debounceMs ?? 200;
  }

  getSnapshot = (target: NoteDocumentTarget): NotePersistenceSnapshot => this.entry(target);

  subscribeNotes = (target: NoteDocumentTarget, listener: () => void): (() => void) => {
    const entry = this.entry(target);
    entry.listeners.add(listener);
    return () => entry.listeners.delete(listener);
  };

  loadNotes = async (
    target: NoteDocumentTarget,
    options: LoadNotesOptions = {},
  ): Promise<NotePersistenceSnapshot> => {
    const entry = this.entry(target);
    const loadRequest = ++entry.loadRequest;
    const revisionAtStart = entry.revision;
    entry.loadState = 'loading';
    entry.loadError = null;
    this.emit(entry);

    try {
      const stored = await this.loadDocument(target);
      if (loadRequest !== entry.loadRequest) return entry;
      const hasNewerDraft = entry.revision !== revisionAtStart
        || entry.revision > entry.acknowledgedRevision;
      if (!hasNewerDraft) {
        entry.document = stored ?? options.createEmpty?.() ?? null;
        if (stored) {
          entry.revision = Math.max(1, entry.revision);
          entry.acknowledgedRevision = entry.revision;
          entry.saveState = 'saved';
          this.persistDraft(entry);
        }
      }
      entry.loadState = 'ready';
      entry.loadError = null;
    } catch (error) {
      if (loadRequest !== entry.loadRequest) return entry;
      entry.loadState = 'error';
      entry.loadError = error;
    }
    this.emit(entry);
    return entry;
  };

  saveNotes = (target: NoteDocumentTarget, document: LiveNotesDocument): number => {
    const entry = this.entry(target);
    entry.document = document;
    entry.revision += 1;
    entry.saveError = null;
    entry.saveState = entry.writePromise ? 'saving' : 'unsaved';
    this.persistDraft(entry);
    this.emit(entry);
    if (entry.saveTimer) clearTimeout(entry.saveTimer);
    entry.saveTimer = setTimeout(() => {
      entry.saveTimer = null;
      void this.startWrite(entry).catch(() => {});
    }, this.debounceMs);
    return entry.revision;
  };

  retryNotes = async (target: NoteDocumentTarget): Promise<void> => {
    const entry = this.entry(target);
    entry.saveError = null;
    await this.flushEntry(entry);
  };

  flushNotes = async (scope?: FlushNotesScope | NoteDocumentTarget): Promise<void> => {
    const matching = [...this.entries.values()].filter((entry) => this.matches(entry.target, scope));
    const results = await Promise.allSettled(matching.map((entry) => this.flushEntry(entry)));
    const failed = results.find((result): result is PromiseRejectedResult => result.status === 'rejected');
    if (failed) throw failed.reason;
  };

  private entry(target: NoteDocumentTarget): NoteEntry {
    const id = noteDocumentId(target);
    const current = this.entries.get(id);
    if (current) return current;

    const entry: NoteEntry = {
      target,
      document: null,
      loadState: 'idle',
      saveState: 'saved',
      revision: 0,
      acknowledgedRevision: 0,
      loadError: null,
      saveError: null,
      listeners: new Set(),
      loadedFallback: false,
      saveTimer: null,
      writePromise: null,
      loadRequest: 0,
    };
    this.entries.set(id, entry);
    this.restoreDraft(entry);
    return entry;
  }

  private restoreDraft(entry: NoteEntry): void {
    if (entry.loadedFallback || !this.storage) return;
    entry.loadedFallback = true;
    try {
      let raw = this.storage.getItem(draftKey(entry.target));
      if (!raw && entry.target.kind === 'live'
        && this.storage.getItem(LIVE_NOTES_FALLBACK_FOLDER_KEY) === entry.target.folderPath) {
        const legacy = this.storage.getItem(LIVE_NOTES_FALLBACK_KEY);
        if (legacy) {
          raw = JSON.stringify({ revision: 1, acknowledgedRevision: 0, document: JSON.parse(legacy) });
          this.storage.setItem(draftKey(entry.target), raw);
          this.storage.removeItem(LIVE_NOTES_FALLBACK_KEY);
          this.storage.removeItem(LIVE_NOTES_FALLBACK_FOLDER_KEY);
        }
      }
      if (!raw) return;
      const draft = JSON.parse(raw) as DraftEnvelope;
      if (!draft.document || !Number.isFinite(draft.revision)) return;
      entry.document = draft.document;
      entry.revision = draft.revision;
      entry.acknowledgedRevision = Math.min(draft.acknowledgedRevision ?? 0, draft.revision);
      entry.saveState = entry.revision > entry.acknowledgedRevision ? 'unsaved' : 'saved';
    } catch {
      // A corrupt recovery draft must not prevent loading the authoritative copy.
    }
  }

  private persistDraft(entry: NoteEntry): void {
    if (!this.storage || !entry.document) return;
    try {
      const envelope: DraftEnvelope = {
        revision: entry.revision,
        acknowledgedRevision: entry.acknowledgedRevision,
        document: entry.document,
      };
      this.storage.setItem(draftKey(entry.target), JSON.stringify(envelope));
    } catch (error) {
      entry.saveError = error;
      entry.saveState = 'error';
    }
  }

  private async startWrite(entry: NoteEntry): Promise<void> {
    if (entry.writePromise) return entry.writePromise;
    if (!entry.document || entry.revision <= entry.acknowledgedRevision) return;
    if (entry.saveTimer) {
      clearTimeout(entry.saveTimer);
      entry.saveTimer = null;
    }

    const revision = entry.revision;
    const document = entry.document;
    entry.saveState = 'saving';
    entry.saveError = null;
    this.emit(entry);

    const write = this.saveDocument(entry.target, document)
      .then(() => {
        entry.acknowledgedRevision = Math.max(entry.acknowledgedRevision, revision);
        entry.saveState = entry.revision === entry.acknowledgedRevision ? 'saved' : 'unsaved';
        this.persistDraft(entry);
      })
      .catch((error) => {
        entry.saveError = error;
        entry.saveState = 'error';
        throw error;
      })
      .finally(() => {
        entry.writePromise = null;
        this.emit(entry);
      });
    entry.writePromise = write;
    return write;
  }

  private async flushEntry(entry: NoteEntry): Promise<void> {
    if (entry.saveTimer) {
      clearTimeout(entry.saveTimer);
      entry.saveTimer = null;
    }
    while (entry.revision > entry.acknowledgedRevision) {
      await this.startWrite(entry);
    }
  }

  private matches(target: NoteDocumentTarget, scope?: FlushNotesScope | NoteDocumentTarget): boolean {
    if (!scope) return true;
    if ('kind' in scope) return noteDocumentId(target) === noteDocumentId(scope);
    if (target.kind === 'meeting') {
      if (scope.meetingId) return target.meetingId === scope.meetingId;
      return !scope.sessionId;
    }
    if (scope.sessionId) return target.sessionId === scope.sessionId;
    return !scope.meetingId;
  }

  private emit(entry: NoteEntry): void {
    entry.listeners.forEach((listener) => listener());
  }
}

export const notePersistenceService = new NotePersistenceService();

export const loadNotes = notePersistenceService.loadNotes;
export const subscribeNotes = notePersistenceService.subscribeNotes;
export const saveNotes = notePersistenceService.saveNotes;
export const retryNotes = notePersistenceService.retryNotes;
export const flushNotes = notePersistenceService.flushNotes;
