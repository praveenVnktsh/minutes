import { afterAll, afterEach, describe, expect, mock, test } from 'bun:test';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import type { ReactNode } from 'react';
import type { MeetingMetadata, StoredTranscript } from '../../src/services/indexedDBService';

const originalDialog = { ...await import('../../src/components/ui/dialog') };
const originalButton = { ...await import('../../src/components/ui/button') };
const originalScrollArea = { ...await import('../../src/components/ui/scroll-area') };
const originalAlert = { ...await import('../../src/components/ui/alert') };

afterAll(() => {
  mock.module('../../src/components/ui/dialog', () => originalDialog);
  mock.module('../../src/components/ui/button', () => originalButton);
  mock.module('../../src/components/ui/scroll-area', () => originalScrollArea);
  mock.module('../../src/components/ui/alert', () => originalAlert);
});

function PassThrough({ children }: { children?: ReactNode }) {
  return <div>{children}</div>;
}

mock.module('../../src/components/ui/dialog', () => ({
  Dialog: PassThrough,
  DialogContent: PassThrough,
  DialogDescription: PassThrough,
  DialogFooter: PassThrough,
  DialogHeader: PassThrough,
  DialogTitle: PassThrough,
}));
mock.module('../../src/components/ui/button', () => ({
  Button: ({ children, ...props }: React.ButtonHTMLAttributes<HTMLButtonElement>) => (
    <button {...props}>{children}</button>
  ),
}));
mock.module('../../src/components/ui/scroll-area', () => ({ ScrollArea: PassThrough }));
mock.module('../../src/components/ui/alert', () => ({ Alert: PassThrough, AlertDescription: PassThrough }));

const { TranscriptRecovery } = await import('../../src/components/TranscriptRecovery/TranscriptRecovery');

const meetings: MeetingMetadata[] = ['slow', 'fast'].map((meetingId) => ({
  meetingId,
  title: meetingId,
  startTime: Date.now() - 60_000,
  lastUpdated: Date.now() - 60_000,
  transcriptCount: 1,
  savedToSQLite: false,
}));

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((resolvePromise) => { resolve = resolvePromise; });
  return { promise, resolve };
}

let renderer: ReactTestRenderer | undefined;

afterEach(async () => {
  if (renderer) await act(async () => renderer!.unmount());
  renderer = undefined;
});

describe('TranscriptRecovery preview', () => {
  test('ignores a stale preview response after a newer meeting is selected', async () => {
    const slow = deferred<StoredTranscript[]>();
    const fast = deferred<StoredTranscript[]>();
    const onLoadPreview = mock((meetingId: string) => meetingId === 'slow' ? slow.promise : fast.promise);
    await act(async () => {
      renderer = create(
        <TranscriptRecovery
          isOpen
          onClose={() => {}}
          recoverableMeetings={meetings}
          onRecover={async () => ({ success: true })}
          onDelete={async () => {}}
          onLoadPreview={onLoadPreview}
        />,
      );
    });
    const meetingButtons = renderer!.root.findAllByType('button').filter((button) => (
      button.props.className?.includes('text-left')
    ));
    await act(async () => { meetingButtons[1].props.onClick(); });
    await act(async () => {
      fast.resolve([{ meetingId: 'fast', text: 'new preview', timestamp: '', confidence: 1, sequenceId: 1, storedAt: 1 }]);
      await fast.promise;
    });
    await act(async () => {
      slow.resolve([{ meetingId: 'slow', text: 'stale preview', timestamp: '', confidence: 1, sequenceId: 1, storedAt: 1 }]);
      await slow.promise;
    });

    const output = JSON.stringify(renderer!.toJSON());
    expect(output).toContain('new preview');
    expect(output).not.toContain('stale preview');
  });
});
