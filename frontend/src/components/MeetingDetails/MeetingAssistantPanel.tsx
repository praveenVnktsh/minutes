'use client';

import { FormEvent, KeyboardEvent, useEffect, useRef, useState } from 'react';
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

interface AssistantResponse {
  message: ChatMessage;
  notesMarkdown?: string | null;
  transcriptEditsApplied: number;
}

const pendingChats = new Map<string, Promise<AssistantResponse>>();
const pendingChatInputs = new Map<string, string>();
const chatErrors = new Map<string, string>();

const STARTERS = [
  'What decisions did we make?',
  'Turn this into clear action items',
  'Make the notes more concise',
];

export function MeetingAssistantPanel({
  meetingId,
  modelConfig,
  onNotesUpdated,
  onTranscriptUpdated,
}: {
  meetingId: string;
  modelConfig: ModelConfig;
  onNotesUpdated: (markdown: string) => void;
  onTranscriptUpdated?: () => Promise<void>;
}) {
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [input, setInput] = useState('');
  const [isSending, setIsSending] = useState(false);
  const [sendError, setSendError] = useState<string | null>(() => chatErrors.get(meetingId) ?? null);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const { isModelConfigSaving } = useConfig();
  const endRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    let cancelled = false;
    invoke<ChatMessage[]>('get_meeting_chat', { meetingId })
      .then((result) => { if (!cancelled) setMessages(result); })
      .catch((error) => console.warn('Could not load meeting chat:', error));
    const pending = pendingChats.get(meetingId);
    if (pending) {
      setIsSending(true);
      void pending.then(async () => {
        const result = await invoke<ChatMessage[]>('get_meeting_chat', { meetingId });
        if (!cancelled) setMessages(result);
      }).catch((error) => {
        if (!cancelled) {
          setSendError(String(error));
          setInput(pendingChatInputs.get(meetingId) ?? '');
        }
      }).finally(() => {
        if (!cancelled) setIsSending(false);
      });
    }
    return () => { cancelled = true; };
  }, [meetingId]);

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
    chatErrors.delete(meetingId);
    try {
      const request = invoke<AssistantResponse>('chat_with_meeting', { meetingId, message: content });
      pendingChats.set(meetingId, request);
      pendingChatInputs.set(meetingId, content);
      const response = await request;
      setMessages((current) => [...current.filter((item) => item.id !== optimistic.id), optimistic, response.message]);
      if (response.notesMarkdown) onNotesUpdated(response.notesMarkdown);
      if (response.transcriptEditsApplied > 0) {
        await onTranscriptUpdated?.();
        toast.success(`Updated ${response.transcriptEditsApplied} transcript segment${response.transcriptEditsApplied === 1 ? '' : 's'}`);
      } else if (response.notesMarkdown) {
        toast.success('Enhanced notes updated');
      }
    } catch (error) {
      chatErrors.set(meetingId, String(error));
      setSendError(String(error));
      setMessages((current) => current.filter((item) => item.id !== optimistic.id));
      setInput(content);
      toast.error('The meeting assistant could not respond', { description: String(error) });
    } finally {
      pendingChats.delete(meetingId);
      pendingChatInputs.delete(meetingId);
      setIsSending(false);
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
          <p className="mt-0.5 text-[11px] text-[var(--ink-subtle)]">Works across notes and transcript</p>
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
          {messages.length === 0 && (
            <div className="py-8">
              <div className="flex h-10 w-10 items-center justify-center rounded-xl bg-[#e2e9df] text-[#55735c]">
                <Bot className="h-5 w-5" />
              </div>
              <h3 className="mt-4 text-sm font-semibold text-ink">Ask about this meeting</h3>
              <p className="mt-1.5 text-xs leading-5 text-[var(--ink-muted)]">Ask questions or tell AI to revise the enhanced notes or transcript.</p>
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
              <Sparkles className="h-4 w-4 animate-pulse" /> Working across the meeting…
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
            placeholder="Ask, summarize, or tell AI what to change…"
            className="max-h-32 min-h-10 flex-1 resize-none bg-transparent px-2 py-2 text-xs leading-5 text-ink outline-none placeholder:text-[var(--ink-subtle)]"
          />
          <button type="submit" disabled={!input.trim() || isSending} className="flex h-9 w-9 shrink-0 items-center justify-center rounded-xl bg-brand text-brand-foreground disabled:opacity-40" aria-label="Send message">
            <ArrowUp className="h-4 w-4" />
          </button>
        </div>
        <p className="mt-2 text-center text-[10px] text-[var(--ink-subtle)]">Transcript edits keep revision history.</p>
      </form>
    </div>
  );
}
