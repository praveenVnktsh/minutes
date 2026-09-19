'use client';

import { Suspense } from 'react';
import { SettingsPageContent } from '@/components/settings/SettingsPageContent';

function SettingsPageFallback() {
  return <div role="status" className="flex h-screen items-center justify-center bg-surface-2 text-sm text-ink-muted">Loading settings…</div>;
}

export default function SettingsPage() {
  return <Suspense fallback={<SettingsPageFallback />}><SettingsPageContent /></Suspense>;
}
