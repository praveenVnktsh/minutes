'use client'

import { useCallback, useEffect, useLayoutEffect, useRef, useState } from 'react'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { invoke } from '@tauri-apps/api/core'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { BellOff, Clock, LoaderCircle, Mic, MoreHorizontal, RotateCcw, Undo2, Video, X } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { StatusFeedback } from '@/components/ui/status-feedback'
import { meetingActivityService } from '@/services/meetingActivityService'
import type { RecordingRequestResult, RecordingStartRequest } from '@/types/meetingActivity'
import { meetingDetectionService, type AudioApp } from '@/services/meetingDetectionService'
import { applyTheme, listenForThemeChanges, readTheme, syncNativeTheme } from '@/lib/theme'

const useBrowserLayoutEffect = typeof window === 'undefined' ? useEffect : useLayoutEffect

/** How long "Won't ask again" stays up with its Undo before the prompt closes. */
export const IGNORE_UNDO_MS = 5000
const SNOOZE_MINUTES = 60

type PromptView = 'prompt' | 'more' | 'ignored'

export default function MeetingPromptPage() {
  const [appName, setAppName] = useState('')
  const [detectedApp, setDetectedApp] = useState<AudioApp | null>(null)
  const [view, setView] = useState<PromptView>('prompt')
  const [actionError, setActionError] = useState<string | null>(null)
  const undoTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null)
  const [requestId, setRequestId] = useState<string | null>(null)
  const [pending, setPending] = useState(false)
  const [listenerReady, setListenerReady] = useState(false)
  const [subscriptionError, setSubscriptionError] = useState<string | null>(null)
  const [listenerAttempt, setListenerAttempt] = useState(0)
  const [error, setError] = useState<string | null>(null)
  const requestIdRef = useRef<string | null>(null)
  const pendingRef = useRef(false)
  const earlyResultsRef = useRef(new Map<string, RecordingRequestResult>())

  const finish = useCallback(async (result: RecordingRequestResult) => {
    if (result.accepted) {
      await getCurrentWindow().hide()
      pendingRef.current = false
      setPending(false)
      setError(null)
      return
    }
    pendingRef.current = false
    setPending(false)
    setError(result.error || 'Recording could not be started.')
  }, [])

  useBrowserLayoutEffect(() => {
    const theme = readTheme()
    applyTheme(theme)
    document.body.style.visibility = 'visible'
    void syncNativeTheme(theme)
  }, [])

  useEffect(() => {
    let disposed = false
    const unlisteners: UnlistenFn[] = []
    const retain = async (registration: Promise<UnlistenFn>, onReady?: () => void, reportFailure = false) => {
      try {
        const unlisten = await registration
        if (disposed) unlisten()
        else {
          unlisteners.push(unlisten)
          onReady?.()
        }
      } catch (registrationError) {
        if (!disposed && reportFailure) {
          setSubscriptionError(registrationError instanceof Error ? registrationError.message : String(registrationError))
        }
      }
    }
    void retain(listenForThemeChanges((theme) => applyTheme(theme)))
    void retain(listen<{ appName: string; app?: AudioApp }>('meeting-prompt-show', ({ payload }) => {
      if (undoTimerRef.current) clearTimeout(undoTimerRef.current)
      undoTimerRef.current = null
      setAppName(payload.appName)
      setDetectedApp(payload.app ?? { name: payload.appName, bundle_id: null })
      setView('prompt')
      setActionError(null)
      setError(null)
      setPending(false)
      pendingRef.current = false
      setRequestId(null)
      requestIdRef.current = null
      earlyResultsRef.current.clear()
    }))
    void retain(meetingActivityService.onPromptStartResult((result) => {
      if (result.request_id === requestIdRef.current) void finish(result)
      else earlyResultsRef.current.set(result.request_id, result)
    }), () => setListenerReady(true), true)
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape' && !pendingRef.current) {
        void invoke('dismiss_meeting_prompt')
          .catch(() => {})
          .then(() => getCurrentWindow().hide())
      }
    }
    window.addEventListener('keydown', handleKeyDown)
    return () => {
      disposed = true
      unlisteners.forEach((unlisten) => unlisten())
      window.removeEventListener('keydown', handleKeyDown)
    }
  }, [finish, listenerAttempt])

  const dismiss = async () => {
    if (pending) return
    await invoke('dismiss_meeting_prompt').catch(() => {})
    await getCurrentWindow().hide()
  }

  useEffect(() => () => {
    if (undoTimerRef.current) clearTimeout(undoTimerRef.current)
  }, [])

  const closeAfterIgnore = useCallback(async () => {
    undoTimerRef.current = null
    await invoke('dismiss_meeting_prompt').catch(() => {})
    await getCurrentWindow().hide()
    setView('prompt')
  }, [])

  const dontAskAgain = async () => {
    if (pending) return
    const target = detectedApp ?? { name: appName, bundle_id: null }
    try {
      await meetingDetectionService.ignoreApp(target)
    } catch (ignoreError) {
      setActionError(ignoreError instanceof Error ? ignoreError.message : String(ignoreError))
      setView('prompt')
      return
    }
    setView('ignored')
    if (undoTimerRef.current) clearTimeout(undoTimerRef.current)
    undoTimerRef.current = setTimeout(() => { void closeAfterIgnore() }, IGNORE_UNDO_MS)
  }

  const undoIgnore = async () => {
    if (undoTimerRef.current) clearTimeout(undoTimerRef.current)
    undoTimerRef.current = null
    const target = detectedApp ?? { name: appName, bundle_id: null }
    try {
      await meetingDetectionService.unignoreApp(target)
      setView('prompt')
    } catch (undoError) {
      setActionError(undoError instanceof Error ? undoError.message : String(undoError))
      setView('prompt')
    }
  }

  const snooze = async () => {
    if (pending) return
    try {
      await meetingDetectionService.snooze(SNOOZE_MINUTES)
      await getCurrentWindow().hide()
      setView('prompt')
    } catch (snoozeError) {
      setActionError(snoozeError instanceof Error ? snoozeError.message : String(snoozeError))
      setView('prompt')
    }
  }

  const startRecording = async () => {
    if (pending) return
    if (!listenerReady) {
      if (subscriptionError) {
        setSubscriptionError(null)
        setListenerAttempt((attempt) => attempt + 1)
      }
      return
    }
    pendingRef.current = true
    setPending(true)
    setError(null)
    try {
      const request = await invoke<RecordingStartRequest>('start_recording_from_prompt')
      requestIdRef.current = request.request_id
      setRequestId(request.request_id)
      const earlyResult = earlyResultsRef.current.get(request.request_id)
      if (earlyResult) {
        earlyResultsRef.current.delete(request.request_id)
        await finish(earlyResult)
      }
    } catch (invokeError) {
      pendingRef.current = false
      setPending(false)
      setError(invokeError instanceof Error ? invokeError.message : String(invokeError))
    }
  }

  const visibleError = subscriptionError ?? error ?? actionError
  const shell = 'flex h-screen w-screen items-center gap-3 rounded-2xl border border-hairline bg-surface-raised px-3 shadow-2xl'

  if (view === 'ignored') {
    return (
      <main className={shell}>
        <span className="flex h-8 w-8 shrink-0 items-center justify-center rounded-full bg-surface-2 text-ink-muted"><BellOff className="h-4 w-4" /></span>
        <p role="status" className="min-w-0 flex-1 truncate text-[12px] text-ink">Won&apos;t ask for {appName} again</p>
        <Button size="sm" variant="outline" className="h-7 shrink-0 gap-1.5 px-3" onClick={() => void undoIgnore()}>
          <Undo2 className="h-3 w-3" />
          Undo
        </Button>
      </main>
    )
  }

  if (view === 'more') {
    return (
      <main className={shell}>
        <div className="flex min-w-0 flex-1 flex-col gap-1">
          <button type="button" onClick={() => void dontAskAgain()} className="flex min-w-0 items-center gap-1.5 rounded-md px-1.5 py-0.5 text-left text-[12px] text-ink hover:bg-surface-2">
            <BellOff className="h-3 w-3 shrink-0 text-ink-subtle" />
            <span className="truncate">Don&apos;t ask for {appName}</span>
          </button>
          <button type="button" onClick={() => void snooze()} className="flex min-w-0 items-center gap-1.5 rounded-md px-1.5 py-0.5 text-left text-[12px] text-ink-muted hover:bg-surface-2 hover:text-ink">
            <Clock className="h-3 w-3 shrink-0 text-ink-subtle" />
            <span className="truncate">Snooze detection for 1 hour</span>
          </button>
        </div>
        <button type="button" aria-label="Back" onClick={() => setView('prompt')} className="shrink-0 rounded-md p-1.5 text-ink-subtle hover:bg-surface-2 hover:text-ink">
          <X className="h-3.5 w-3.5" />
        </button>
      </main>
    )
  }

  return (
    <main className={shell}>
      <span className="flex h-8 w-8 shrink-0 items-center justify-center rounded-full bg-surface-2 text-ink-muted"><Video className="h-4 w-4" /></span>
      <div className="min-w-0 flex-1">
        <p className="truncate text-[13px] font-medium text-ink">Meeting detected</p>
        {visibleError ? <StatusFeedback tone="error" className="line-clamp-1 text-[10px]">{visibleError}</StatusFeedback> : (
          <p className="truncate text-[11px] text-ink-subtle">{pending ? `Starting recording${requestId ? '…' : ' request…'}` : appName}</p>
        )}
      </div>
      <Button size="sm" className="h-7 shrink-0 gap-1.5 px-3" disabled={pending || (!listenerReady && !subscriptionError)} onClick={() => void startRecording()}>
        {pending ? <LoaderCircle className="h-3 w-3 animate-spin" /> : (subscriptionError ?? error) ? <RotateCcw className="h-3 w-3" /> : <Mic className="h-3 w-3" />}
        {pending ? 'Starting' : !listenerReady ? subscriptionError ? 'Retry' : 'Preparing' : error ? 'Retry' : 'Start'}
      </Button>
      <div className="-ml-1 flex shrink-0 items-center">
        <button type="button" aria-label="More options" disabled={pending} onClick={() => setView('more')} className="rounded-md p-1.5 text-ink-subtle hover:bg-surface-2 hover:text-ink disabled:opacity-40">
          <MoreHorizontal className="h-3.5 w-3.5" />
        </button>
        <button type="button" aria-label="Dismiss" disabled={pending} onClick={() => void dismiss()} className="rounded-md p-1.5 text-ink-subtle hover:bg-surface-2 hover:text-ink disabled:opacity-40">
          <X className="h-3.5 w-3.5" />
        </button>
      </div>
    </main>
  )
}
