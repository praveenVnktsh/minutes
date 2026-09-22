import { useCallback, useEffect, useRef, useState } from 'react';

export interface DownloadDetail {
  downloadedMb: number;
  totalMb: number;
  speedMbps: number;
}

export type CancelOutcome = 'cancelled' | 'pending';

/** Engine download events, normalised by each manager's source. */
export type ModelDownloadEvent =
  | { kind: 'progress'; model: string; progress: number; detail?: DownloadDetail; completed?: boolean }
  | { kind: 'complete'; model: string }
  | { kind: 'error'; model: string; error: string }
  | { kind: 'cancelled'; model: string };

type ProgressEvent = Extract<ModelDownloadEvent, { kind: 'progress' }>;

/** One per engine. Define it at module level so it is referentially stable. */
export interface ModelDownloadSource {
  /** Resolves to a single unlisten function; rejects if registration failed. */
  subscribe(emit: (event: ModelDownloadEvent) => void): Promise<() => void>;
  start(model: string): Promise<void>;
  cancel(model: string): Promise<CancelOutcome | void>;
}

/**
 * Registers several Tauri listeners as one. If any registration rejects, the
 * ones that succeeded are unlistened and that rejection reason is rethrown.
 */
export async function listenAll(registrations: Array<Promise<() => void>>): Promise<() => void> {
  const results = await Promise.allSettled(registrations);
  const unlisteners = results.flatMap(result => (result.status === 'fulfilled' ? [result.value] : []));
  const failed = results.find((result): result is PromiseRejectedResult => result.status === 'rejected');
  if (failed) {
    unlisteners.forEach(unlisten => unlisten());
    throw failed.reason;
  }
  return () => unlisteners.forEach(unlisten => unlisten());
}

export interface ModelDownloadHandlers {
  /** After the busy guard passes, before source.start. */
  onStart?(model: string): void;
  /** Only for updates that pass the throttle. */
  onProgress?(model: string, progress: number, event: ProgressEvent): void;
  /** The handlers below run after the hook has settled the model. */
  onComplete?(model: string): void;
  onError?(model: string, error: string): void;
  onCancelled?(model: string): void;
  /** source.start rejected. */
  onStartFailed?(model: string, error: unknown): void;
}

export interface UseModelDownloadOptions extends ModelDownloadHandlers {
  source: ModelDownloadSource;
  /** When set, the in-flight set is loaded from and written to localStorage[persistKey]. */
  persistKey?: string;
}

export interface ModelDownload {
  downloading: ReadonlySet<string>;
  cancelling: ReadonlySet<string>;
  progress: Readonly<Record<string, number>>;
  detail: Readonly<Record<string, DownloadDetail>>;
  listenersReady: boolean;
  listenError: string | null;
  isBusy(model: string): boolean;
  download(model: string): Promise<boolean>;
  cancel(model: string): Promise<CancelOutcome | void>;
  markCancelling(model: string): void;
  settle(model: string): void;
}

const THROTTLE_MS = 300;
const THROTTLE_STEP = 5;

function readPersisted(key: string | undefined): Set<string> {
  if (!key) return new Set();
  try {
    const saved = globalThis.localStorage?.getItem(key);
    const parsed: unknown = saved ? JSON.parse(saved) : [];
    return new Set(Array.isArray(parsed) ? parsed.filter((m): m is string => typeof m === 'string') : []);
  } catch {
    return new Set();
  }
}

function writePersisted(key: string | undefined, models: ReadonlySet<string>) {
  if (!key) return;
  try {
    globalThis.localStorage?.setItem(key, JSON.stringify(Array.from(models)));
  } catch {
    // Storage unavailable or full; the in-memory state is still correct.
  }
}

function withoutKey<T>(record: Record<string, T>, key: string): Record<string, T> {
  if (!(key in record)) return record;
  const { [key]: _removed, ...rest } = record;
  return rest;
}

function listenErrorMessage(reason: unknown): string {
  if (reason instanceof Error) return reason.message;
  if (typeof reason === 'string') return reason;
  return 'Failed to listen for model download updates';
}

export function useModelDownload(options: UseModelDownloadOptions): ModelDownload {
  const { source, persistKey } = options;

  // The listener is registered once, so it reads handlers and config through refs.
  const optionsRef = useRef(options);
  optionsRef.current = options;

  const [downloading, setDownloading] = useState<ReadonlySet<string>>(() => readPersisted(persistKey));
  const [cancelling, setCancelling] = useState<ReadonlySet<string>>(() => new Set());
  const [progress, setProgress] = useState<Record<string, number>>({});
  const [detail, setDetail] = useState<Record<string, DownloadDetail>>({});
  const [listenersReady, setListenersReady] = useState(false);
  const [listenError, setListenError] = useState<string | null>(null);

  // Refs mirror the sets synchronously so isBusy is never render-stale.
  const downloadingRef = useRef(downloading);
  const cancellingRef = useRef(cancelling);
  const throttleRef = useRef(new Map<string, { progress: number; timestamp: number }>());

  const updateDownloading = useCallback((model: string, present: boolean) => {
    const current = downloadingRef.current;
    if (current.has(model) === present) return;
    const next = new Set(current);
    if (present) next.add(model);
    else next.delete(model);
    downloadingRef.current = next;
    setDownloading(next);
    writePersisted(optionsRef.current.persistKey, next);
  }, []);

  const updateCancelling = useCallback((model: string, present: boolean) => {
    const current = cancellingRef.current;
    if (current.has(model) === present) return;
    const next = new Set(current);
    if (present) next.add(model);
    else next.delete(model);
    cancellingRef.current = next;
    setCancelling(next);
  }, []);

  const isBusy = useCallback(
    (model: string) => downloadingRef.current.has(model) || cancellingRef.current.has(model),
    []
  );

  const settle = useCallback((model: string) => {
    updateDownloading(model, false);
    updateCancelling(model, false);
    throttleRef.current.delete(model);
    setProgress(prev => withoutKey(prev, model));
    setDetail(prev => withoutKey(prev, model));
  }, [updateDownloading, updateCancelling]);

  const markCancelling = useCallback((model: string) => updateCancelling(model, true), [updateCancelling]);

  const download = useCallback(async (model: string) => {
    if (isBusy(model)) return false;
    updateDownloading(model, true);
    optionsRef.current.onStart?.(model);
    try {
      await optionsRef.current.source.start(model);
    } catch (err) {
      settle(model);
      optionsRef.current.onStartFailed?.(model, err);
    }
    return true;
  }, [isBusy, updateDownloading, settle]);

  const cancel = useCallback(async (model: string) => {
    updateCancelling(model, true);
    try {
      return await optionsRef.current.source.cancel(model);
    } catch (err) {
      updateCancelling(model, false);
      throw err;
    }
  }, [updateCancelling]);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | null = null;

    const handleProgress = (event: ProgressEvent) => {
      const { model } = event;
      // A progress event for an unknown model means a download is already in
      // flight (e.g. the manager remounted), so restore it.
      updateDownloading(model, true);

      const now = Date.now();
      const last = throttleRef.current.get(model);
      const pass = !last ||
        now - last.timestamp > THROTTLE_MS ||
        Math.abs(event.progress - last.progress) >= THROTTLE_STEP ||
        event.progress >= 100 ||
        event.completed === true;
      if (!pass) return;

      throttleRef.current.set(model, { progress: event.progress, timestamp: now });
      setProgress(prev => ({ ...prev, [model]: event.progress }));
      if (event.detail) {
        const eventDetail = event.detail;
        setDetail(prev => ({ ...prev, [model]: eventDetail }));
      }
      optionsRef.current.onProgress?.(model, event.progress, event);
    };

    const handleEvent = (event: ModelDownloadEvent) => {
      if (disposed) return;
      switch (event.kind) {
        case 'progress':
          handleProgress(event);
          return;
        case 'complete':
          settle(event.model);
          optionsRef.current.onComplete?.(event.model);
          return;
        case 'error':
          settle(event.model);
          optionsRef.current.onError?.(event.model, event.error);
          return;
        case 'cancelled':
          settle(event.model);
          optionsRef.current.onCancelled?.(event.model);
          return;
      }
    };

    source.subscribe(handleEvent).then(
      fn => {
        if (disposed) {
          fn();
          return;
        }
        unlisten = fn;
        setListenError(null);
        setListenersReady(true);
      },
      reason => {
        if (disposed) return;
        setListenError(listenErrorMessage(reason));
      }
    );

    return () => {
      disposed = true;
      unlisten?.();
      setListenersReady(false);
    };
  }, [source, settle, updateDownloading]);

  return {
    downloading,
    cancelling,
    progress,
    detail,
    listenersReady,
    listenError,
    isBusy,
    download,
    cancel,
    markCancelling,
    settle,
  };
}
