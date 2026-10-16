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

/** Which final-pass prompt the card edits: for meetings with notes, or without. */
export type SummaryPromptKind = 'notes' | 'transcriptOnly';

const COPY: Record<
  SummaryPromptKind,
  { title: string; description: string; noun: string }
> = {
  notes: {
    title: 'Notes prompt',
    description:
      'The instruction sent to the model for meetings where you typed notes: it enriches your notes from the transcript. Edit it to change the tone, structure or focus of generated notes. It applies to summaries generated after you save.',
    noun: 'Notes prompt',
  },
  transcriptOnly: {
    title: 'No-notes prompt',
    description:
      'The instruction sent to the model for meetings without notes: it writes notes from the transcript alone. It applies to summaries generated after you save.',
    noun: 'No-notes prompt',
  },
};

/** Remembers that the user has seen the notes/no-notes split notice. */
export const PROMPT_SPLIT_NOTICE_KEY = 'summaryPromptSplitNoticeDismissed';

function noticeDismissed(): boolean {
  try {
    return typeof localStorage !== 'undefined' && localStorage.getItem(PROMPT_SPLIT_NOTICE_KEY) === 'true';
  } catch {
    return false;
  }
}

function dismissNotice() {
  try {
    localStorage.setItem(PROMPT_SPLIT_NOTICE_KEY, 'true');
  } catch {
    // Without storage the notice just shows again next time.
  }
}

type LoadState =
  | { status: 'loading' }
  | { status: 'error'; message: string }
  | { status: 'ready'; saved: SummaryPromptSettingsData };

/**
 * Settings card for a system prompt sent to the model when it writes meeting notes. The
 * Rust core owns the default and the saved override; this card only edits and resets it.
 */
export function SummaryPromptSettings({ kind = 'notes' }: { kind?: SummaryPromptKind }) {
  const copy = COPY[kind];
  const descriptionId = `summary-prompt-description-${kind}`;
  const [state, setState] = useState<LoadState>({ status: 'loading' });
  const [draft, setDraft] = useState('');
  const [busy, setBusy] = useState<'saving' | 'resetting' | null>(null);
  // Before the split, a custom notes prompt applied to every meeting. For a
  // user who had one, say once that meetings without notes now use this
  // prompt, and offer to carry their text over.
  const [customNotesPrompt, setCustomNotesPrompt] = useState<string | null>(null);

  const applySettings = useCallback((settings: SummaryPromptSettingsData) => {
    setState({ status: 'ready', saved: settings });
    setDraft(settings.prompt);
  }, []);

  const load = useCallback(async () => {
    setState({ status: 'loading' });
    try {
      applySettings(await invoke<SummaryPromptSettingsData>('api_get_summary_prompt', { kind }));
    } catch (error) {
      setState({ status: 'error', message: String(error) });
    }
  }, [applySettings, kind]);

  useEffect(() => {
    void load();
  }, [load]);

  const savedIsCustom = state.status === 'ready' ? state.saved.isCustom : null;
  useEffect(() => {
    if (kind !== 'transcriptOnly' || savedIsCustom !== false || noticeDismissed()) return;
    let cancelled = false;
    invoke<SummaryPromptSettingsData>('api_get_summary_prompt', { kind: 'notes' })
      .then((notes) => {
        if (!cancelled && notes.isCustom) setCustomNotesPrompt(notes.prompt);
      })
      .catch(() => {
        // The notice is only a hint; without it the card still works.
      });
    return () => {
      cancelled = true;
    };
  }, [kind, savedIsCustom]);

  const closeNotice = () => {
    dismissNotice();
    setCustomNotesPrompt(null);
  };

  const save = async () => {
    setBusy('saving');
    try {
      const savedSettings = await invoke<SummaryPromptSettingsData>('api_save_summary_prompt', {
        prompt: draft,
        kind,
      });
      applySettings(savedSettings);
      if (kind === 'transcriptOnly' && savedSettings.isCustom) dismissNotice();
      toast.success(`${copy.noun} saved`);
    } catch (error) {
      toast.error(`Could not save the ${copy.noun.toLowerCase()}`, { description: String(error) });
    } finally {
      setBusy(null);
    }
  };

  const reset = async () => {
    setBusy('resetting');
    try {
      applySettings(await invoke<SummaryPromptSettingsData>('api_reset_summary_prompt', { kind }));
      toast.success(`${copy.noun} reset to default`);
    } catch (error) {
      toast.error(`Could not reset the ${copy.noun.toLowerCase()}`, { description: String(error) });
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
          <h3 className="text-lg font-semibold text-ink">{copy.title}</h3>
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
      <p id={descriptionId} className="mb-4 text-sm text-ink-muted">
        {copy.description}
      </p>

      {state.status === 'loading' && <p className="text-sm text-ink-muted">Loading prompt…</p>}

      {state.status === 'error' && (
        <div role="alert" className={`flex items-center justify-between gap-4 rounded-md p-3 text-sm ${panel.error}`}>
          <span>
            Could not load the {copy.noun.toLowerCase()}: {state.message}
          </span>
          <Button variant="outline" size="sm" onClick={() => void load()}>
            Retry
          </Button>
        </div>
      )}

      {saved && customNotesPrompt !== null && !saved.isCustom && (
        <div
          data-testid="prompt-split-notice"
          className={`mb-4 rounded-md p-3 text-sm ${panel.info}`}
        >
          <p>
            Your custom Notes prompt now applies only to meetings where you typed notes. Meetings
            without notes use this No-notes prompt, which is still the default.
          </p>
          <div className="mt-2 flex flex-wrap gap-2">
            <Button
              size="sm"
              variant="outline"
              onClick={() => {
                // Remembered only once the user saves: leaving without
                // saving keeps the offer for next time.
                setDraft(customNotesPrompt);
                setCustomNotesPrompt(null);
              }}
            >
              Start from my Notes prompt
            </Button>
            <Button size="sm" variant="ghost" onClick={closeNotice}>
              Keep the default
            </Button>
          </div>
        </div>
      )}

      {saved && (
        <>
          <Textarea
            aria-label={copy.title}
            aria-describedby={descriptionId}
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
