'use client';

import { FormEvent, KeyboardEvent, useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { ArrowUp, Bot, ChevronDown, Sparkles } from 'lucide-react';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import { toast } from 'sonner';
import { ModelConfig, ModelSettingsModal } from '@/components/ModelSettingsModal';
import { Dialog, DialogContent, DialogTitle, DialogTrigger } from '@/components/ui/dialog';
import { VisuallyHidden } from '@/components/ui/visually-hidden';
import { useConfig } from '@/contexts/ConfigContext';

interface ChatMessage {
  id: string;
  role: 'user' | 'assistant';
  content: string;
  createdAt: string;
}

interface ChatOperation {
  status: 'pending' | 'result' | 'error';
  draft: string;
  promise: Promise<ChatMessage>;
  result?: ChatMessage;
  error?: string;
  optimisticId?: string;
}

const chatOperations = new Map<string, ChatOperation>();

const STARTERS = [
  'What decisions did we make?',
  'Turn this into clear action items',
  'What open questions are still unresolved?',
];

export function MeetingAssistantPanel({
  meetingId,
  modelConfig,
}: {
  meetingId: string;
  modelConfig: ModelConfig;
}) {
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [input, setInput] = useState('');
  const [isSending, setIsSending] = useState(false);
  const [sendError, setSendError] = useState<string | null>(() => chatOperations.get(meetingId)?.error ?? null);
  const [historyState, setHistoryState] = useState<'loading' | 'ready' | 'error'>('loading');
  const [historyError, setHistoryError] = useState<string | null>(null);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const { isModelConfigSaving } = useConfig();
  const endRef = useRef<HTMLDivElement>(null);
  const mountedRef = useRef(true);
  const historyRequestRef = useRef(0);
  const restoredDraftRef = useRef<string | null>(null);

  const loadHistory = useCallback(async () => {
    const requestId = ++historyRequestRef.current;
    setHistoryState('loading');
    setHistoryError(null);
    try {
      const result = await invoke<ChatMessage[]>('get_meeting_chat', { meetingId });
      if (!mountedRef.current || historyRequestRef.current !== requestId) return;
      setMessages(result);
      setHistoryState('ready');
    } catch (error) {
      if (!mountedRef.current || historyRequestRef.current !== requestId) return;
      console.warn('Could not load meeting chat:', error);
      setHistoryError(String(error));
      setHistoryState('error');
    }
  }, [meetingId]);

  const applyResult = useCallback(async (operation: ChatOperation) => {
    if (!mountedRef.current || chatOperations.get(meetingId) !== operation) return;
    await loadHistory();
    if (!mountedRef.current || chatOperations.get(meetingId) !== operation) return;
    setInput((current) => current === restoredDraftRef.current ? '' : current);
    restoredDraftRef.current = null;
    chatOperations.delete(meetingId);
    setIsSending(false);
  }, [loadHistory, meetingId]);

  const applyError = useCallback((operation: ChatOperation, error: unknown) => {
    if (!mountedRef.current || chatOperations.get(meetingId) !== operation) return;
    const message = String(error);
    operation.status = 'error';
    operation.error = message;
    setSendError(message);
    setInput(operation.draft);
    if (operation.optimisticId) {
      setMessages((current) => current.filter((message) => message.id !== operation.optimisticId));
    }
    setIsSending(false);
  }, [meetingId]);

  useEffect(() => {
    mountedRef.current = true;
    void loadHistory();
    const operation = chatOperations.get(meetingId);
    if (operation?.status === 'pending') {
      setIsSending(true);
      restoredDraftRef.current = operation.draft;
      setInput(operation.draft);
      void operation.promise.then(() => applyResult(operation)).catch((error) => applyError(operation, error));
    } else if (operation?.status === 'result' && operation.result) {
      setIsSending(true);
      void applyResult(operation);
    } else if (operation?.status === 'error') {
      setSendError(operation.error ?? 'Unknown error');
      restoredDraftRef.current = operation.draft;
      setInput(operation.draft);
    }
    return () => { mountedRef.current = false; };
  }, [applyError, applyResult, loadHistory, meetingId]);

  useEffect(() => {
    endRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [messages, isSending]);

  const send = async (event?: FormEvent) => {
    event?.preventDefault();
    const content = input.trim();
    if (!content || isSending) return;
    const optimistic: ChatMessage = {
      id: `pending-${Date.now()}`,
      role: 'user',
      content,
      createdAt: new Date().toISOString(),
    };
    setMessages((current) => [...current, optimistic]);
    setInput('');
    setIsSending(true);
    setSendError(null);
    const request = invoke<ChatMessage>('chat_with_meeting', { meetingId, message: content });
    const operation: ChatOperation = { status: 'pending', draft: content, promise: request, optimisticId: optimistic.id };
    chatOperations.set(meetingId, operation);
    try {
      const response = await request;
      operation.status = 'result';
      operation.result = response;
      await applyResult(operation);
    } catch (error) {
      applyError(operation, error);
      toast.error('The meeting assistant could not respond', { description: String(error) });
    }
  };

  const onKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    if (event.key === 'Enter' && !event.shiftKey) {
      event.preventDefault();
      void send();
    }
  };

  return (
    <div className="flex h-full flex-col bg-[var(--surface-1)]">
      <div className="flex items-start justify-between gap-3 px-5 pb-3 pt-5">
        <div>
          <h2 className="text-sm font-semibold text-ink">AI chat</h2>
          <p className="mt-0.5 text-[11px] text-[var(--ink-subtle)]">Answers from the notes and transcript</p>
        </div>
        <Dialog open={settingsOpen} onOpenChange={(open) => { if (!isModelConfigSaving) setSettingsOpen(open); }}>
          <DialogTrigger asChild>
            <button type="button" className="flex h-8 max-w-[180px] items-center gap-1 rounded-full bg-surface-2 px-3 text-[11px] text-ink-muted hover:bg-surface-raised" title="Choose AI model">
              <span className="truncate">{modelConfig.model || 'Choose model'}</span>
              <ChevronDown className="h-3 w-3 shrink-0 text-[var(--ink-subtle)]" />
            </button>
          </DialogTrigger>
          <DialogContent aria-describedby={undefined}>
            <VisuallyHidden><DialogTitle>AI model settings</DialogTitle></VisuallyHidden>
            <ModelSettingsModal
              onCommitted={() => setSettingsOpen(false)}
              onCancel={() => setSettingsOpen(false)}
              layout="dialog"
            />
          </DialogContent>
        </Dialog>
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto px-5 py-3">
        <div className="space-y-5">
          {historyState === 'loading' && (
            <p role="status" className="py-8 text-xs text-ink-muted">Loading conversation…</p>
          )}
          {historyState === 'error' && (
            <div className="space-y-3 py-8" role="alert">
              <p className="text-xs text-error">Could not load conversation: {historyError}</p>
              <button type="button" onClick={() => void loadHistory()} className="rounded-full border border-hairline px-3 py-1.5 text-xs text-ink">Retry</button>
            </div>
          )}
          {historyState === 'ready' && messages.length === 0 && (
            <div className="py-8">
              <div className="flex h-10 w-10 items-center justify-center rounded-xl bg-[#e2e9df] text-[#55735c]">
                <Bot className="h-5 w-5" />
              </div>
              <h3 className="mt-4 text-sm font-semibold text-ink">Ask about this meeting</h3>
              <p className="mt-1.5 text-xs leading-5 text-[var(--ink-muted)]">Ask questions about the enhanced notes or transcript.</p>
              <div className="mt-5 flex flex-col items-start gap-2">
                {STARTERS.map((starter) => (
                  <button key={starter} type="button" onClick={() => setInput(starter)} className="rounded-full bg-surface-2 px-3 py-1.5 text-left text-[11px] text-ink-muted hover:bg-surface-raised">
                    {starter}
                  </button>
                ))}
              </div>
            </div>
          )}
          {messages.map((message) => (
            <div key={message.id} className={message.role === 'user' ? 'flex justify-end' : 'flex justify-start'}>
              <div className={message.role === 'user' ? 'max-w-[88%] rounded-2xl rounded-br-md bg-brand px-3.5 py-2.5 text-xs leading-5 text-brand-foreground' : 'prose prose-sm max-w-full text-xs leading-5 text-[var(--ink-muted)]'}>
                {message.role === 'assistant'
                  ? <ReactMarkdown remarkPlugins={[remarkGfm]}>{message.content}</ReactMarkdown>
                  : message.content}
              </div>
            </div>
          ))}
          {isSending && (
            <div className="flex items-center gap-2 text-sm text-[var(--ink-subtle)]">
              <Sparkles className="h-4 w-4 animate-pulse" /> Reading the meeting…
            </div>
          )}
          {sendError && <p role="alert" className="text-xs text-error">The meeting assistant could not respond: {sendError}</p>}
          <div ref={endRef} />
        </div>
      </div>
      <form onSubmit={send} className="px-4 pb-5 pt-3">
        <div className="flex items-end gap-2 rounded-2xl bg-surface-0 p-2 shadow-[0_6px_24px_rgba(45,43,37,0.08)]">
          <textarea
            value={input}
            onChange={(event) => setInput(event.target.value)}
            onKeyDown={onKeyDown}
            rows={1}
            placeholder="Ask about the notes or transcript…"
            className="max-h-32 min-h-10 flex-1 resize-none bg-transparent px-2 py-2 text-xs leading-5 text-ink outline-none placeholder:text-[var(--ink-subtle)]"
          />
          <button type="submit" disabled={!input.trim() || isSending} className="flex h-9 w-9 shrink-0 items-center justify-center rounded-xl bg-brand text-brand-foreground disabled:opacity-40" aria-label="Send message">
            <ArrowUp className="h-4 w-4" />
          </button>
        </div>
        <p className="mt-2 text-center text-[10px] text-[var(--ink-subtle)]">Answers from the notes and transcript without changing them.</p>
      </form>
    </div>
  );
}
