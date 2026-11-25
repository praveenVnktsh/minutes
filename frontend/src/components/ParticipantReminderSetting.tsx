'use client';

import { useEffect, useRef, useState } from 'react';
import { Switch } from '@/components/ui/switch';
import { SaveFeedback, StatusFeedback, type SaveFeedbackState } from '@/components/ui/status-feedback';

/**
 * The "remind me to tell participants" toast shown when a recording starts
 * (lib/recordingNotification.tsx). Stored in the Tauri preferences store, where
 * that toast's own "Don't show this again" also writes it.
 */
export function ParticipantReminderSetting() {
  const [enabled, setEnabled] = useState(true);
  const [saveState, setSaveState] = useState<SaveFeedbackState | null>(null);
  const [failureMessage, setFailureMessage] = useState('Could not save participant reminder; previous setting restored');
  const [hydration, setHydration] = useState<'loading' | 'ready' | 'error'>('loading');
  const revision = useRef(0);
  const saving = useRef(false);

  useEffect(() => {
    const load = async () => {
      const loadRevision = revision.current;
      try {
        const { Store } = await import('@tauri-apps/plugin-store');
        const store = await Store.load('preferences.json');
        const show = await store.get<boolean>('show_recording_notification') ?? true;
        if (loadRevision === revision.current) {
          setEnabled(show);
          setHydration('ready');
        }
      } catch (error) {
        console.error('Failed to load notification preference:', error);
        if (loadRevision === revision.current) setHydration('error');
      }
    };
    void load();
  }, []);

  const toggle = async (next: boolean) => {
    if (saving.current || hydration !== 'ready') return;
    saving.current = true;
    revision.current += 1;
    const previous = enabled;
    setEnabled(next);
    setSaveState('saving');
    setFailureMessage('Could not save participant reminder; previous setting restored');
    let store: { set: (key: string, value: unknown) => Promise<void>; save: () => Promise<void> } | null = null;
    try {
      const { Store } = await import('@tauri-apps/plugin-store');
      store = await Store.load('preferences.json');
      await store.set('show_recording_notification', next);
      await store.save();
      setSaveState('saved');
    } catch (error) {
      console.error('Failed to save notification preference:', error);
      setEnabled(previous);
      if (store) {
        try {
          await store.set('show_recording_notification', previous);
          await store.save();
        } catch (restoreError) {
          console.error('Failed to restore notification preference:', restoreError);
          setFailureMessage('Could not save or restore the participant reminder; reopen settings to verify it');
        }
      }
      setSaveState('error');
    } finally {
      saving.current = false;
    }
  };

  return (
    <div className="space-y-3">
      <div className="flex items-center justify-between gap-4">
        <div>
          <p className="text-sm font-medium text-ink">Remind me to tell participants</p>
          <p className="text-sm text-ink-muted">When a recording starts, show a reminder to let everyone know it is being recorded</p>
        </div>
        <Switch
          aria-label="Remind me to inform participants when recording starts"
          checked={enabled}
          onCheckedChange={(value) => void toggle(value)}
          disabled={hydration !== 'ready' || saveState === 'saving'}
        />
      </div>
      {hydration === 'loading' && <StatusFeedback pending>Loading participant reminder preference</StatusFeedback>}
      {hydration === 'error' && <StatusFeedback tone="error">Could not load participant reminder preference</StatusFeedback>}
      {saveState && (
        <SaveFeedback
          state={saveState}
          labels={{
            saving: 'Saving participant reminder',
            saved: 'Participant reminder saved',
            error: failureMessage,
          }}
        />
      )}
    </div>
  );
}
