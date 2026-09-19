'use client'

import { useCallback, useEffect, useLayoutEffect, useRef, useState } from 'react'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { invoke } from '@tauri-apps/api/core'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { LoaderCircle, Mic, RotateCcw, Video, X } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { StatusFeedback } from '@/components/ui/status-feedback'
import { meetingActivityService } from '@/services/meetingActivityService'
import type { RecordingRequestResult, RecordingStartRequest } from '@/types/meetingActivity'
import { applyTheme, listenForThemeChanges, readTheme, syncNativeTheme } from '@/lib/theme'

const useBrowserLayoutEffect = typeof window === 'undefined' ? useEffect : useLayoutEffect

export default function MeetingPromptPage() {
  const [appName, setAppName] = useState('')
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
    void retain(listen<{ appName: string }>('meeting-prompt-show', ({ payload }) => {
      setAppName(payload.appName)
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

  const visibleError = subscriptionError ?? error

  return (
    <main className="flex h-screen w-screen items-center gap-3 rounded-2xl border border-hairline bg-surface-raised px-3 shadow-2xl">
      <span className="flex h-8 w-8 shrink-0 items-center justify-center rounded-full bg-surface-2 text-ink-muted"><Video className="h-4 w-4" /></span>
      <div className="min-w-0 flex-1">
        <p className="truncate text-[13px] font-medium text-ink">Meeting detected</p>
        {visibleError ? <StatusFeedback tone="error" className="line-clamp-1 text-[10px]">{visibleError}</StatusFeedback> : (
          <p className="truncate text-[11px] text-ink-subtle">{pending ? `Starting recording${requestId ? '…' : ' request…'}` : appName}</p>
        )}
      </div>
      <Button size="sm" className="h-7 shrink-0 gap-1.5 px-3" disabled={pending || (!listenerReady && !subscriptionError)} onClick={() => void startRecording()}>
        {pending ? <LoaderCircle className="h-3 w-3 animate-spin" /> : visibleError ? <RotateCcw className="h-3 w-3" /> : <Mic className="h-3 w-3" />}
        {pending ? 'Starting' : !listenerReady ? subscriptionError ? 'Retry' : 'Preparing' : error ? 'Retry' : 'Start'}
      </Button>
      <button type="button" aria-label="Dismiss" disabled={pending} onClick={() => void dismiss()} className="shrink-0 rounded-md p-1.5 text-ink-subtle hover:bg-surface-2 hover:text-ink disabled:opacity-40">
        <X className="h-3.5 w-3.5" />
      </button>
    </main>
  )
}
