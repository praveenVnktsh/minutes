import { describe, expect, it } from 'bun:test';
import type { LiveNotesDocument } from '@/lib/liveNotes';
import {
  liveNotesTarget,
  meetingNotesTarget,
  NotePersistenceService,
} from './notePersistenceService';

function document(text: string): LiveNotesDocument {
  return {
    version: 2,
    meetingStartedAtMs: 1,
    updatedAt: text,
    notes: [{ id: 'note', timestampSeconds: 0, text, important: false }],
    rawMarkdown: text,
  };
}

function memoryStorage() {
  const values = new Map<string, string>();
  return {
    values,
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => { values.set(key, value); },
    removeItem: (key: string) => { values.delete(key); },
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

describe('NotePersistenceService', () => {
  it('serializes overlapping writes and only marks the latest acknowledged revision saved', async () => {
    const writes: Array<{ value: LiveNotesDocument; result: ReturnType<typeof deferred<void>> }> = [];
    const service = new NotePersistenceService({
      storage: memoryStorage(),
      debounceMs: 60_000,
      save: async (_target, value) => {
        const result = deferred<void>();
        writes.push({ value, result });
        await result.promise;
      },
    });
    const target = meetingNotesTarget('one');

    service.saveNotes(target, document('first'));
    const flushing = service.flushNotes(target);
    await Promise.resolve();
    expect(writes.map(({ value }) => value.rawMarkdown)).toEqual(['first']);

    service.saveNotes(target, document('latest'));
    expect(service.getSnapshot(target).saveState).toBe('saving');
    writes[0].result.resolve();
    while (writes.length < 2) await new Promise((resolve) => setTimeout(resolve, 0));
    expect(service.getSnapshot(target).saveState).not.toBe('saved');
    expect(writes.map(({ value }) => value.rawMarkdown)).toEqual(['first', 'latest']);

    writes[1].result.resolve();
    await flushing;
    expect(service.getSnapshot(target).acknowledgedRevision).toBe(2);
    expect(service.getSnapshot(target).saveState).toBe('saved');
  });

  it('keeps a rejected save as a durable draft and retries the latest revision', async () => {
    const storage = memoryStorage();
    let attempts = 0;
    const service = new NotePersistenceService({
      storage,
      debounceMs: 60_000,
      save: async () => {
        attempts += 1;
        if (attempts === 1) throw new Error('disk unavailable');
      },
    });
    const target = liveNotesTarget('/meeting/one');
    service.saveNotes(target, document('recover me'));

    await expect(service.flushNotes(target)).rejects.toThrow('disk unavailable');
    expect(service.getSnapshot(target).saveState).toBe('error');
    expect([...storage.values.values()].join('')).toContain('recover me');

    await service.retryNotes(target);
    expect(attempts).toBe(2);
    expect(service.getSnapshot(target).saveState).toBe('saved');
  });

  it('does not create a blank replacement when loading fails', async () => {
    let attempts = 0;
    const service = new NotePersistenceService({
      storage: memoryStorage(),
      load: async () => {
        attempts += 1;
        if (attempts === 1) throw new Error('read failed');
        return document('recovered');
      },
    });
    const target = meetingNotesTarget('failed-load');

    await service.loadNotes(target, { createEmpty: () => document('blank') });

    expect(service.getSnapshot(target).loadState).toBe('error');
    expect(service.getSnapshot(target).document).toBeNull();

    await service.loadNotes(target, { createEmpty: () => document('blank') });
    expect(service.getSnapshot(target).loadState).toBe('ready');
    expect(service.getSnapshot(target).document?.rawMarkdown).toBe('recovered');
  });

  it('does not let a late disk read overwrite edits made while loading', async () => {
    const read = deferred<LiveNotesDocument | null>();
    const service = new NotePersistenceService({
      storage: memoryStorage(),
      debounceMs: 60_000,
      load: () => read.promise,
      save: async () => {},
    });
    const target = meetingNotesTarget('load-race');
    const loading = service.loadNotes(target);
    service.saveNotes(target, document('typed while loading'));
    read.resolve(document('stale disk'));

    await loading;
    expect(service.getSnapshot(target).document?.rawMarkdown).toBe('typed while loading');
    await service.flushNotes(target);
  });

  it('scopes recovery drafts and flushes to their document identity', async () => {
    const storage = memoryStorage();
    const saved: string[] = [];
    const firstService = new NotePersistenceService({
      storage,
      debounceMs: 60_000,
      save: async (target) => { saved.push(target.kind === 'meeting' ? target.meetingId : target.sessionId); },
    });
    const meetingOne = meetingNotesTarget('one');
    const meetingTwo = meetingNotesTarget('two');
    firstService.saveNotes(meetingOne, document('meeting one'));
    firstService.saveNotes(meetingTwo, document('meeting two'));
    firstService.saveNotes(liveNotesTarget('/live/session'), document('live session'));

    await firstService.flushNotes({ meetingId: 'two' });
    expect(saved).toEqual(['two']);
    expect(firstService.getSnapshot(meetingOne).saveState).toBe('unsaved');
    expect(firstService.getSnapshot(liveNotesTarget('/live/session')).saveState).toBe('unsaved');

    const recoveredService = new NotePersistenceService({ storage, debounceMs: 60_000 });
    expect(recoveredService.getSnapshot(meetingOne).document?.rawMarkdown).toBe('meeting one');
    expect(recoveredService.getSnapshot(meetingTwo).document?.rawMarkdown).toBe('meeting two');
    expect(recoveredService.getSnapshot(meetingOne).saveState).toBe('unsaved');
    expect(recoveredService.getSnapshot(meetingTwo).saveState).toBe('saved');
  });

  it('prefers an unsaved crash-recovery draft over an older disk document', async () => {
    const storage = memoryStorage();
    const target = meetingNotesTarget('crash');
    const beforeCrash = new NotePersistenceService({ storage, debounceMs: 60_000 });
    beforeCrash.saveNotes(target, document('new local draft'));

    const afterCrash = new NotePersistenceService({
      storage,
      load: async () => document('old disk copy'),
    });
    await afterCrash.loadNotes(target);

    expect(afterCrash.getSnapshot(target).document?.rawMarkdown).toBe('new local draft');
    expect(afterCrash.getSnapshot(target).saveState).toBe('unsaved');
  });
});
