import { afterEach, describe, expect, test } from 'bun:test';
import { IndexedDBService, type MeetingMetadata, type StoredTranscript } from './indexedDBService';

interface FakeOptions {
  meeting?: MeetingMetadata;
  transcripts?: StoredTranscript[];
  getError?: Error;
  putError?: Error;
  transactionError?: Error;
  transactionThrows?: Error;
}

interface FakeRequest {
  result?: unknown;
  error: Error | null;
  onsuccess: ((event: Event) => void) | null;
  onerror: ((event: Event) => void) | null;
}

interface FakeTransaction {
  error: Error | null;
  oncomplete: ((event: Event) => void) | null;
  onerror: ((event: Event) => void) | null;
  onabort: ((event: Event) => void) | null;
  objectStore?: () => IDBObjectStore;
}

function fakeDatabase(options: FakeOptions = {}): IDBDatabase {
  // Stateful across transactions so a put() made through one service call
  // is visible to a get() made through a later one (round-trip tests).
  let storedMeeting = options.meeting;
  return {
    transaction: () => {
      if (options.transactionThrows) throw options.transactionThrows;
      const transaction: FakeTransaction = {
        error: options.transactionError ?? null,
        oncomplete: null,
        onerror: null,
        onabort: null,
      };
      const finish = () => queueMicrotask(() => {
        if (options.transactionError) transaction.onerror?.(new Event('error'));
        else transaction.oncomplete?.(new Event('complete'));
      });
      const store = {
        get: () => {
          const request: FakeRequest = {
            error: options.getError ?? null,
            onsuccess: null,
            onerror: null,
          };
          queueMicrotask(() => {
            if (options.getError) request.onerror?.(new Event('error'));
            else {
              request.result = storedMeeting;
              request.onsuccess?.(new Event('success'));
              if (!storedMeeting) finish();
            }
          });
          return request as unknown as IDBRequest;
        },
        put: (value: unknown) => {
          const request: FakeRequest = {
            error: options.putError ?? null,
            onsuccess: null,
            onerror: null,
          };
          queueMicrotask(() => {
            if (options.putError) request.onerror?.(new Event('error'));
            else {
              storedMeeting = value as MeetingMetadata;
              request.onsuccess?.(new Event('success'));
            }
            finish();
          });
          return request as unknown as IDBRequest;
        },
        index: () => ({
          getAll: () => {
            const request: FakeRequest = {
              error: options.getError ?? null,
              onsuccess: null,
              onerror: null,
            };
            queueMicrotask(() => {
              if (options.getError) request.onerror?.(new Event('error'));
              else {
                request.result = options.transcripts ?? [];
                request.onsuccess?.(new Event('success'));
              }
              finish();
            });
            return request as unknown as IDBRequest;
          },
        }),
      };
      transaction.objectStore = () => store as unknown as IDBObjectStore;
      return transaction as unknown as IDBTransaction;
    },
  } as unknown as IDBDatabase;
}

const meeting: MeetingMetadata = {
  meetingId: 'recovery-1',
  title: 'Recovered meeting',
  startTime: 1,
  lastUpdated: 1,
  transcriptCount: 1,
  savedToSQLite: false,
};

afterEach(() => {
  delete (globalThis as { indexedDB?: IDBFactory }).indexedDB;
});

describe('IndexedDBService strict recovery operations', () => {
  test('strict transcript reads distinguish an empty result from request failure', async () => {
    await expect(new IndexedDBService(fakeDatabase()).getTranscriptsStrict('recovery-1'))
      .resolves.toEqual([]);
    await expect(new IndexedDBService(fakeDatabase({ getError: new Error('read failed') }))
      .getTranscriptsStrict('recovery-1')).rejects.toThrow('read failed');
  });

  test('strict transcript reads reject initialization and transaction failures', async () => {
    Object.defineProperty(globalThis, 'indexedDB', {
      configurable: true,
      value: { open: () => { throw new Error('open failed'); } },
    });
    await expect(new IndexedDBService().getTranscriptsStrict('recovery-1'))
      .rejects.toThrow('open failed');
    await expect(new IndexedDBService(fakeDatabase({ transactionError: new Error('tx failed') }))
      .getTranscriptsStrict('recovery-1')).rejects.toThrow('tx failed');
  });

  test('strict mark rejects missing records and get or put failures', async () => {
    await expect(new IndexedDBService(fakeDatabase()).markMeetingSavedStrict('missing'))
      .rejects.toThrow('was not found');
    await expect(new IndexedDBService(fakeDatabase({ meeting, getError: new Error('get failed') }))
      .markMeetingSavedStrict('recovery-1')).rejects.toThrow('get failed');
    await expect(new IndexedDBService(fakeDatabase({ meeting, putError: new Error('put failed') }))
      .markMeetingSavedStrict('recovery-1')).rejects.toThrow('put failed');
  });

  test('strict mark waits for and rejects transaction completion failure', async () => {
    await expect(new IndexedDBService(fakeDatabase({
      meeting: { ...meeting },
      transactionError: new Error('commit failed'),
    })).markMeetingSavedStrict('recovery-1')).rejects.toThrow('commit failed');
  });

  test('legacy methods remain tolerant for existing callers', async () => {
    const service = new IndexedDBService(fakeDatabase({ getError: new Error('legacy failure') }));

    await expect(service.getTranscripts('recovery-1')).resolves.toEqual([]);
    await expect(service.markMeetingSaved('recovery-1')).resolves.toBeUndefined();
  });
});

describe('IndexedDBService resumeOfMeetingId', () => {
  test('setResumeOfMeetingId stamps an existing record, keeping its other fields, and the stamp reads back', async () => {
    const service = new IndexedDBService(fakeDatabase({ meeting: { ...meeting } }));

    await expect(service.setResumeOfMeetingId('recovery-1', 'meeting-100')).resolves.toBe(true);
    await expect(service.getMeetingMetadata('recovery-1')).resolves.toEqual({
      ...meeting,
      resumeOfMeetingId: 'meeting-100',
    });
  });

  test('setResumeOfMeetingId resolves false and writes nothing for a missing record', async () => {
    // No `meeting` given, so the record is missing; a putError would fail the
    // test if the implementation wrongly issued a put in this branch.
    const service = new IndexedDBService(fakeDatabase({ putError: new Error('should not be called') }));

    await expect(service.setResumeOfMeetingId('missing', 'meeting-100')).resolves.toBe(false);
  });

  test('getMeetingMetadata loads a record without resumeOfMeetingId with the field undefined', async () => {
    const service = new IndexedDBService(fakeDatabase({ meeting: { ...meeting } }));

    const loaded = await service.getMeetingMetadata('recovery-1');
    expect(loaded?.resumeOfMeetingId).toBeUndefined();
  });

  test('saveMeetingMetadata preserves an existing resumeOfMeetingId when incoming metadata omits it', async () => {
    const service = new IndexedDBService(fakeDatabase({
      meeting: { ...meeting, resumeOfMeetingId: 'meeting-100' },
    }));
    const incoming: MeetingMetadata = { ...meeting, transcriptCount: 5 };

    await service.saveMeetingMetadata(incoming);

    await expect(service.getMeetingMetadata('recovery-1')).resolves.toEqual({
      ...meeting,
      transcriptCount: 5,
      resumeOfMeetingId: 'meeting-100',
    });
  });
});
