'use client';

import { FormEvent, KeyboardEvent, useEffect, useMemo, useRef, useState } from 'react';
import { ArrowUp, Copy, Minimize2, RotateCw, Sparkles, Square } from 'lucide-react';
import { toast } from 'sonner';
import { Switch } from '@/components/ui/switch';
import { CitedMarkdown } from '@/components/MeetingDetails/CitedMarkdown';
import { useCopilot } from '@/hooks/useCopilot';
import { writeClipboardText } from '@/lib/clipboard';
import {
  QUICK_ACTIONS,
  cardHeader,
  copilotApiFor,
  debugLine,
  type CopilotMessage,
  type QuickActionId,
} from '@/lib/copilot';
import { buildLiveNotesSummaryContext } from '@/lib/liveNotes';
import { badge, panel } from '@/lib/theme-classes';
import { useLiveSource } from '@/lib/liveSource';
import { noteDocumentId, notePersistenceService, type NoteDocumentTarget } from '@/services/notePersistenceService';

/** Copilot's commands through this subtree's live source. */
function useCopilotApi() {
  const source = useLiveSource();
  return useMemo(() => copilotApiFor(source), [source]);
}

/** Keeps Copilot's copy of the user's live notes current (debounced). */
function useNotesForCopilot(target: NoteDocumentTarget | undefined) {
  const copilotApi = useCopilotApi();
  const targetRef = useRef(target);
  targetRef.current = target;
  const key = target ? noteDocumentId(target) : null;
  useEffect(() => {
    const target = targetRef.current;
    if (!key || !target) return undefined;
    let timer: ReturnType<typeof setTimeout> | null = null;
    let last = '';
    const push = () => {
      const text = buildLiveNotesSummaryContext(notePersistenceService.getSnapshot(target).document);
      if (text === last) return;
      last = text;
      void copilotApi.setNotes(text).catch(() => {});
    };
    push();
    const unsubscribe = notePersistenceService.subscribeNotes(target, () => {
      if (timer) clearTimeout(timer);
      timer = setTimeout(push, 1000);
    });
    return () => {
      if (timer) clearTimeout(timer);
      unsubscribe();
    };
  }, [key, copilotApi]);
}

function copy(text: string) {
  void writeClipboardText(text)
    .then(() => toast.success('Copied'))
    .catch((error) => toast.error('Could not copy', { description: String(error) }));
}

const smallButton = 'flex h-7 items-center gap-1 rounded-full px-2.5 text-[11px] text-ink-muted hover:bg-surface-2 hover:text-ink disabled:opacity-40';

function Reply({ message }: { message: CopilotMessage }) {
  const copilotApi = useCopilotApi();
  const working = message.status === 'pending' || message.status === 'streaming';
  if (message.status === 'error') {
    return (
      <div role="alert" className={`rounded-xl px-3 py-2.5 text-xs ${panel.error}`}>
        <p className="font-semibold">Copilot couldn&apos;t get an answer</p>
        {message.error && <p className="mt-1 leading-5">{message.error}</p>}
        <div className="mt-2 flex flex-wrap gap-1.5">
          <button type="button" className="rounded-full border border-error/30 px-2.5 py-1 text-[11px] hover:bg-surface-2" onClick={() => void copilotApi.retry(message.id, false).catch((e) => toast.error(String(e)))}>
            Retry
          </button>
          {message.canUseBuiltin && (
            <button type="button" className="rounded-full border border-error/30 px-2.5 py-1 text-[11px] hover:bg-surface-2" onClick={() => void copilotApi.retry(message.id, true).catch((e) => toast.error(String(e)))}>
              Use built-in model instead
            </button>
          )}
        </div>
      </div>
    );
  }
  return (
    <div>
      {message.reply ? (
        <div className="prose prose-sm max-w-full text-xs leading-5 text-[var(--ink-muted)]">
          <CitedMarkdown>{message.reply}</CitedMarkdown>
        </div>
      ) : working ? (
        <p className="flex items-center gap-2 text-xs text-[var(--ink-subtle)]">
          <Sparkles className="h-3.5 w-3.5 animate-pulse" /> Listening back…
        </p>
      ) : null}
      {message.status === 'stopped' && <p className="mt-1 text-[10px] text-[var(--ink-subtle)]">Stopped</p>}
      <div className="mt-1.5 flex flex-wrap gap-1">
        {working ? (
          <button type="button" className={smallButton} onClick={() => void copilotApi.stop(message.id)}>
            <Square className="h-3 w-3" /> Stop
          </button>
        ) : (
          message.reply && (
            <>
              <button type="button" className={smallButton} onClick={() => copy(message.reply)}>
                <Copy className="h-3 w-3" /> Copy
              </button>
              {message.kind !== 'suggestion' && message.action !== 'shorter' && (
                <button type="button" className={smallButton} onClick={() => void copilotApi.shorter(message.id).catch((e) => toast.error(String(e)))}>
                  <Minimize2 className="h-3 w-3" /> Shorter
                </button>
              )}
            </>
          )
        )}
      </div>
    </div>
  );
}

function SuggestionCard({ message }: { message: CopilotMessage }) {
  const copilotApi = useCopilotApi();
  const active = message.cardState === 'active';
  return (
    <div className={`rounded-xl border px-3 py-2.5 ${active ? 'border-warning/40 bg-surface-0' : 'border-hairline bg-surface-1 opacity-80'}`}>
      <p className={`inline-block rounded px-1.5 py-px text-[10px] font-semibold tracking-[0.06em] ${badge.warning}`}>
        {cardHeader(message)}
      </p>
      <p className="mt-1.5 text-xs italic leading-5 text-ink">&ldquo;{message.prompt}&rdquo;</p>
      <div className="mt-2">
        <Reply message={message} />
      </div>
      {active && (
        <div className="mt-1 flex gap-1 border-t border-hairline pt-1.5">
          <button type="button" className={smallButton} onClick={() => void copilotApi.dismissCard(message.id, true)}>
            Not for me
          </button>
          <button type="button" className={smallButton} onClick={() => void copilotApi.dismissCard(message.id, false)}>
            Dismiss
          </button>
        </div>
      )}
    </div>
  );
}

/**
 * The dock's lower panel while recording: quick actions, questions and
 * proactive suggestions about the last few minutes, streamed from the model.
 * Laid out like the AI chat panel it replaces during the meeting.
 */
export function CopilotPanel({ notesTarget }: { notesTarget?: NoteDocumentTarget }) {
  const copilotApi = useCopilotApi();
  const snapshot = useCopilot();
  const [input, setInput] = useState('');
  const [sending, setSending] = useState(false);
  const [suggestions, setSuggestions] = useState<boolean | null>(null);
  const endRef = useRef<HTMLDivElement>(null);
  useNotesForCopilot(notesTarget);

  const messages = snapshot?.messages ?? [];
  const active = snapshot?.active ?? false;
  const ready = active && (snapshot?.lineCount ?? 0) > 0;
  const suggestionsOn = suggestions ?? snapshot?.suggestionsEnabled ?? true;
  const lastReplyLength = messages[messages.length - 1]?.reply.length ?? 0;

  useEffect(() => {
    endRef.current?.scrollIntoView({ behavior: 'smooth', block: 'end' });
  }, [messages.length, lastReplyLength]);

  const ask = async (action: QuickActionId | null, question: string | null) => {
    if (!ready || sending) return;
    setSending(true);
    try {
      await copilotApi.ask(action, question);
      if (question) setInput('');
    } catch (error) {
      toast.error('Copilot could not start', { description: String(error) });
    } finally {
      setSending(false);
    }
  };

  const submit = (event?: FormEvent) => {
    event?.preventDefault();
    const question = input.trim();
    if (question) void ask(null, question);
  };

  const onKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    if (event.key === 'Enter' && !event.shiftKey) {
      event.preventDefault();
      submit();
    }
  };

  const toggleSuggestions = async (enabled: boolean) => {
    setSuggestions(enabled);
    try {
      const current = await copilotApi.getSettings();
      const saved = await copilotApi.setSettings({ ...current, suggestionsEnabled: enabled });
      setSuggestions(saved.suggestionsEnabled);
    } catch (error) {
      setSuggestions(!enabled);
      toast.error('Could not change suggestions', { description: String(error) });
    }
  };

  return (
    <div className="flex h-full flex-col bg-[var(--surface-1)]">
      <div className="flex items-start justify-between gap-3 px-5 pb-3 pt-5">
        <div>
          <h2 className="text-sm font-semibold text-ink">Copilot</h2>
          <p className="mt-0.5 text-[11px] text-[var(--ink-subtle)]">Live · answers from the last few minutes and the live summary</p>
        </div>
        <label className="flex shrink-0 items-center gap-2 text-[11px] text-ink-muted" title="Suggest a reply when someone asks you something">
          <Switch checked={suggestionsOn} onCheckedChange={(checked) => void toggleSuggestions(checked)} aria-label="Suggestions" />
          Suggestions
        </label>
      </div>

      <div className="flex flex-wrap gap-1.5 px-5 pb-2">
        {QUICK_ACTIONS.map((action) => (
          <button
            key={action.id}
            type="button"
            disabled={!ready || sending}
            onClick={() => void ask(action.id, null)}
            className="rounded-full bg-surface-2 px-3 py-1.5 text-[11px] text-ink-muted hover:bg-surface-raised disabled:opacity-40"
          >
            {action.label}
          </button>
        ))}
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto px-5 py-3">
        <div className="space-y-4">
          {messages.length === 0 && (
            <p className="py-6 text-xs leading-5 text-[var(--ink-muted)]">
              {!snapshot || !active
                ? 'Copilot works while a meeting is recording.'
                : ready
                  ? 'Ask about the last few minutes, or pick a quick action. When someone asks you something, a suggested reply appears here.'
                  : 'Copilot starts once the first words are transcribed.'}
            </p>
          )}
          {messages.map((message) =>
            message.kind === 'suggestion' ? (
              <SuggestionCard key={message.id} message={message} />
            ) : (
              <div key={message.id} className="space-y-2">
                <div className="flex justify-end">
                  <div className="max-w-[88%] rounded-2xl rounded-br-md bg-brand px-3.5 py-2.5 text-xs leading-5 text-brand-foreground">
                    {message.prompt}
                  </div>
                </div>
                <Reply message={message} />
              </div>
            ),
          )}
          <div ref={endRef} />
        </div>
      </div>

      <form onSubmit={submit} className="px-4 pb-5 pt-3">
        <div className="flex items-end gap-2 rounded-2xl bg-surface-0 p-2 shadow-[0_6px_24px_rgba(45,43,37,0.08)]">
          <textarea
            value={input}
            onChange={(event) => setInput(event.target.value)}
            onKeyDown={onKeyDown}
            rows={1}
            disabled={!ready}
            placeholder="Ask about this meeting…"
            className="max-h-32 min-h-10 flex-1 resize-none bg-transparent px-2 py-2 text-xs leading-5 text-ink outline-none placeholder:text-[var(--ink-subtle)] disabled:opacity-60"
          />
          <button type="submit" disabled={!ready || !input.trim() || sending} className="flex h-9 w-9 shrink-0 items-center justify-center rounded-xl bg-brand text-brand-foreground disabled:opacity-40" aria-label="Ask Copilot">
            {sending ? <RotateCw className="h-4 w-4 animate-spin" /> : <ArrowUp className="h-4 w-4" />}
          </button>
        </div>
        {snapshot?.debugEnabled ? (
          <p className="mt-2 break-words font-mono text-[10px] leading-4 text-[var(--ink-subtle)]" aria-label="Copilot debug">
            {debugLine(snapshot)}
          </p>
        ) : (
          <p className="mt-2 text-center text-[10px] text-[var(--ink-subtle)]">Saved to this meeting&apos;s AI chat.</p>
        )}
      </form>
    </div>
  );
}
