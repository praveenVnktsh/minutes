'use client';

import { Suspense } from 'react';
import { useRouter, useSearchParams } from 'next/navigation';
import { RunList } from '@/components/Replay/RunList';
import { ReplayViewer } from '@/components/Replay/ReplayViewer';

function ReplaysContent() {
  const searchParams = useSearchParams();
  const router = useRouter();
  const run = searchParams.get('run');
  if (run) return <ReplayViewer run={run} onBack={() => router.push('/replays')} />;
  return <RunList onOpen={(name) => router.push(`/replays?run=${encodeURIComponent(name)}`)} />;
}

export default function ReplaysPage() {
  return (
    <Suspense fallback={<div role="status" className="flex h-screen items-center justify-center bg-surface-0 text-sm text-ink-muted">Loading replays…</div>}>
      <ReplaysContent />
    </Suspense>
  );
}
