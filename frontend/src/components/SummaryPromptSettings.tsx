'use client';

import { useCallback, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { FileText } from 'lucide-react';
import { toast } from 'sonner';
import { Button } from '@/components/ui/button';
import { Textarea } from '@/components/ui/textarea';
import { badge, panel } from '@/lib/theme-classes';

/** What the Rust core returns for the notes prompt: the effective prompt and the built-in one. */
export interface SummaryPromptSettingsData {
  prompt: string;
  defaultPrompt: string;
  isCustom: boolean;
}

type LoadState =
  | { status: 'loading' }
  | { status: 'error'; message: string }
  | { status: 'ready'; saved: SummaryPromptSettingsData };

/**
 * Settings card for the system prompt sent to the model when it writes meeting notes. The
 * Rust core owns the default and the saved override; this card only edits and resets it.
 */
export function SummaryPromptSettings() {
  const [state, setState] = useState<LoadState>({ status: 'loading' });
  const [draft, setDraft] = useState('');
  const [busy, setBusy] = useState<'saving' | 'resetting' | null>(null);

  const applySettings = useCallback((settings: SummaryPromptSettingsData) => {
    setState({ status: 'ready', saved: settings });
    setDraft(settings.prompt);
  }, []);

  const load = useCallback(async () => {
    setState({ status: 'loading' });
    try {
      applySettings(await invoke<SummaryPromptSettingsData>('api_get_summary_prompt'));
    } catch (error) {
      setState({ status: 'error', message: String(error) });
    }
  }, [applySettings]);

  useEffect(() => {
    void load();
  }, [load]);

  const save = async () => {
    setBusy('saving');
    try {
      applySettings(await invoke<SummaryPromptSettingsData>('api_save_summary_prompt', { prompt: draft }));
      toast.success('Notes prompt saved');
    } catch (error) {
      toast.error('Could not save the notes prompt', { description: String(error) });
    } finally {
      setBusy(null);
    }
  };

  const reset = async () => {
    setBusy('resetting');
    try {
      applySettings(await invoke<SummaryPromptSettingsData>('api_reset_summary_prompt'));
      toast.success('Notes prompt reset to default');
    } catch (error) {
      toast.error('Could not reset the notes prompt', { description: String(error) });
    } finally {
      setBusy(null);
    }
  };

  const saved = state.status === 'ready' ? state.saved : null;
  const dirty = saved !== null && draft !== saved.prompt;
  const canReset = saved !== null && (saved.isCustom || draft.trim() !== saved.defaultPrompt.trim());

  return (
    <div className="rounded-lg border border-hairline bg-surface-raised p-6 shadow-sm">
      <div className="mb-2 flex items-center justify-between gap-4">
        <div className="flex items-center gap-2">
          <FileText size={18} className="text-ink-muted" />
          <h3 className="text-lg font-semibold text-ink">Notes prompt</h3>
        </div>
        {saved && (
          <span
            data-testid="summary-prompt-status"
            className={`rounded-full px-2 py-0.5 text-xs font-medium ${saved.isCustom ? badge.info : badge.neutral}`}
          >
            {saved.isCustom ? 'Custom' : 'Default'}
          </span>
        )}
      </div>
      <p id="summary-prompt-description" className="mb-4 text-sm text-ink-muted">
        The instruction sent to the model when it writes meeting notes. Edit it to change the
        tone, structure or focus of generated notes. It applies to summaries generated after you
        save.
      </p>

      {state.status === 'loading' && <p className="text-sm text-ink-muted">Loading prompt…</p>}

      {state.status === 'error' && (
        <div role="alert" className={`flex items-center justify-between gap-4 rounded-md p-3 text-sm ${panel.error}`}>
          <span>Could not load the notes prompt: {state.message}</span>
          <Button variant="outline" size="sm" onClick={() => void load()}>
            Retry
          </Button>
        </div>
      )}

      {saved && (
        <>
          <Textarea
            aria-label="Notes prompt"
            aria-describedby="summary-prompt-description"
            value={draft}
            onChange={(event) => setDraft(event.target.value)}
            disabled={busy !== null}
            spellCheck={false}
            className="h-72 resize-y font-mono text-xs leading-relaxed md:text-xs"
          />
          <div className="mt-3 flex flex-wrap items-center gap-3">
            <Button onClick={() => void save()} disabled={!dirty || busy !== null}>
              {busy === 'saving' ? 'Saving…' : 'Save prompt'}
            </Button>
            <Button variant="outline" onClick={() => void reset()} disabled={!canReset || busy !== null}>
              {busy === 'resetting' ? 'Resetting…' : 'Reset to default'}
            </Button>
            {dirty && <span className="text-xs text-ink-subtle">Unsaved changes</span>}
          </div>
        </>
      )}
    </div>
  );
}
