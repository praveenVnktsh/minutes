import { convertFileSrc, invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import type { LiveAskRequest, ReplayBackend } from './replaySource';

/** One run in the replays folder (`replay_list_runs`, same as `replay serve`'s
 * `/api/runs`). */
export interface ReplayRun {
  name: string;
  status: 'done' | 'running' | 'incomplete';
  recording: string | null;
  audio_name: string | null;
  duration_s: number | null;
  realtime: boolean | null;
  started_at: string | null;
  events_bytes: number;
  passed: number | null;
  failed: number | null;
  interactive: number;
}

export type ReplayFile = 'events' | 'report' | 'interactive';

/** `replay-copilot` event payload: one step of a live ask. */
export interface ReplayCopilotEvent {
  requestId: string;
  run: string;
  event: string;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  data: any;
}

export const REPLAY_COPILOT_EVENT = 'replay-copilot';

const decoder = new TextDecoder();

function asText(body: unknown): string {
  if (typeof body === 'string') return body;
  if (body instanceof ArrayBuffer) return decoder.decode(body);
  if (ArrayBuffer.isView(body)) return decoder.decode(body);
  if (Array.isArray(body)) return decoder.decode(new Uint8Array(body));
  return '';
}

export const replayApi = {
  listRuns: async (): Promise<ReplayRun[]> => {
    const result = await invoke<{ runs: ReplayRun[] }>('replay_list_runs');
    return result?.runs ?? [];
  },
  readFile: async (run: string, file: ReplayFile): Promise<string> =>
    asText(await invoke<ArrayBuffer>('replay_read_file', { run, file })),
  /** A URL the webview can stream the run's recording from, or null. */
  audioUrl: async (run: string): Promise<string | null> => {
    const path = await invoke<string | null>('replay_audio_path', { run });
    return path ? convertFileSrc(path) : null;
  },
  cancelCopilot: () => invoke<void>('replay_copilot_cancel'),
  onCopilotEvent: (handler: (event: ReplayCopilotEvent) => void) =>
    listen<ReplayCopilotEvent>(REPLAY_COPILOT_EVENT, ({ payload }) => handler(payload)),
};

/** Live asks for `run`, through the Tauri commands. */
export function tauriReplayBackend(run: string): ReplayBackend {
  return {
    ask: (request: LiveAskRequest, requestId: string) =>
      invoke<void>('replay_copilot_ask', { run, request, requestId }),
    stop: () => invoke<boolean>('replay_copilot_stop', { run }),
  };
}
