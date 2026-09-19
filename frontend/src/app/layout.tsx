'use client'

import './globals.css'
import { Source_Sans_3, Source_Serif_4 } from 'next/font/google'
import { useCallback, useEffect, useLayoutEffect, useState } from 'react'
import { usePathname } from 'next/navigation'
import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { AlertTriangle, LoaderCircle, Square } from 'lucide-react'
import SimpleSidebar from '@/components/SimpleSidebar'
import { SidebarProvider } from '@/components/Sidebar/SidebarProvider'
import MainContent from '@/components/MainContent'
import AnalyticsProvider from '@/components/AnalyticsProvider'
import { TooltipProvider } from '@/components/ui/tooltip'
import { Button } from '@/components/ui/button'
import { StatusFeedback } from '@/components/ui/status-feedback'
import { MeetingActivityProvider, useMeetingActivity } from '@/contexts/MeetingActivityContext'
import { RecordingStateProvider, useRecordingState } from '@/contexts/RecordingStateContext'
import { OllamaDownloadProvider } from '@/contexts/OllamaDownloadContext'
import { TranscriptProvider } from '@/contexts/TranscriptContext'
import { ConfigProvider, useConfig } from '@/contexts/ConfigContext'
import { OnboardingProvider } from '@/contexts/OnboardingContext'
import { RecordingControllerProvider, useRecordingController } from '@/contexts/RecordingControllerContext'
import { ShellProvider, useShell } from '@/contexts/ShellContext'
import { ImportDialogProvider } from '@/contexts/ImportDialogContext'
import { OnboardingFlow } from '@/components/onboarding'
import { DownloadProgressToastProvider } from '@/components/shared/DownloadProgressToast'
import { TranscriptionProgressToastProvider } from '@/components/shared/TranscriptionProgressToast'
import { UpdateCheckProvider } from '@/components/UpdateCheckProvider'
import { ImportAudioDialog, ImportDropOverlay } from '@/components/ImportAudio'
import { MeetingDetectedPrompt } from '@/components/MeetingDetectedPrompt'
import { AutoSummaryProvider } from '@/components/AutoSummaryProvider'
import { RecordingControllerFeedback } from '@/components/RecordingControllerFeedback'
import { TranscriptRecoveryMount } from '@/components/TranscriptRecovery/TranscriptRecoveryMount'
import { MeetingActivityFeedback } from '@/app/_components/StatusOverlays'
import { ThemedToaster } from '@/components/ThemedToaster'
import { CommandPalette } from '@/components/CommandPalette'
import { isAudioExtension, getAudioFormatsDisplayList } from '@/constants/audioFormats'
import { applyTheme, readTheme } from '@/lib/theme'
import { settingsHref } from '@/components/settings/settingsSections'
import { toast } from 'sonner'
import 'sonner/dist/styles.css'

const sourceSans3 = Source_Sans_3({
  subsets: ['latin'],
  weight: ['400', '500', '600', '700'],
  variable: '--font-source-sans-3',
})

const sourceSerif4 = Source_Serif_4({
  subsets: ['latin'],
  weight: ['400', '500', '600', '700'],
  variable: '--font-source-serif-4',
})

export type StartupState = 'checking' | 'setup-required' | 'ready' | 'error'

function ThemeBootstrap() {
  useLayoutEffect(() => applyTheme(readTheme()), [])
  return null
}

function BootSurface({ state, onRetry }: { state: StartupState; onRetry: () => void }) {
  if (state === 'setup-required') return null
  return (
    <main className="flex h-screen items-center justify-center bg-surface-0 px-6 text-ink">
      <div className="w-full max-w-md rounded-2xl border border-hairline bg-surface-raised p-8 text-center shadow-sm">
        {state === 'checking' ? (
          <LoaderCircle className="mx-auto h-6 w-6 animate-spin text-info" aria-hidden="true" />
        ) : (
          <AlertTriangle className="mx-auto h-6 w-6 text-error" aria-hidden="true" />
        )}
        <h1 className="mt-4 text-xl font-semibold">{state === 'checking' ? 'Opening Minutes' : 'Minutes could not start'}</h1>
        <p className="mt-2 text-sm text-ink-muted" role={state === 'error' ? 'alert' : 'status'}>
          {state === 'checking' ? 'Checking your local setup…' : 'The setup status could not be read. Your meetings have not been changed.'}
        </p>
        {state === 'error' && <Button className="mt-5" onClick={onRetry}>Retry</Button>}
      </div>
    </main>
  )
}

function ConditionalImportDialog({ open, onOpenChange, filePath }: {
  open: boolean
  onOpenChange: (open: boolean) => void
  filePath: string | null
}) {
  const { betaFeatures } = useConfig()
  if (!betaFeatures.importAndRetranscribe) return null
  return <ImportAudioDialog open={open} onOpenChange={onOpenChange} preselectedFile={filePath} />
}

function ImportSurfaces({ open, onOpenChange, filePath, onFileDrop }: {
  open: boolean
  onOpenChange: (open: boolean) => void
  filePath: string | null
  onFileDrop: (paths: string[]) => void
}) {
  const [showDropOverlay, setShowDropOverlay] = useState(false)
  const { betaFeatures } = useConfig()
  const { navigate } = useShell()

  useEffect(() => {
    let disposed = false
    const unlisteners: UnlistenFn[] = []
    const retain = async (registration: Promise<UnlistenFn>) => {
      const unlisten = await registration
      if (disposed) unlisten()
      else unlisteners.push(unlisten)
    }
    void retain(listen('tauri://drag-enter', () => {
      if (betaFeatures.importAndRetranscribe) setShowDropOverlay(true)
    }))
    void retain(listen('tauri://drag-leave', () => setShowDropOverlay(false)))
    void retain(listen<{ paths: string[] }>('tauri://drag-drop', ({ payload }) => {
      setShowDropOverlay(false)
      if (!betaFeatures.importAndRetranscribe) {
        toast.info('Audio import is a beta feature', {
          description: 'Enable it in Beta settings before importing a recording.',
          action: { label: 'Open settings', onClick: () => void navigate(settingsHref('beta')) },
        })
        return
      }
      onFileDrop(payload.paths)
    }))
    return () => {
      disposed = true
      unlisteners.forEach((unlisten) => unlisten())
    }
  }, [betaFeatures.importAndRetranscribe, navigate, onFileDrop])

  return (
    <>
      <ImportDropOverlay visible={showDropOverlay} />
      <ConditionalImportDialog open={open} onOpenChange={onOpenChange} filePath={filePath} />
    </>
  )
}

function GlobalActivitySurface() {
  const controller = useRecordingController()
  const recordingState = useRecordingState()
  const { snapshot } = useMeetingActivity()
  const visibleActivities = snapshot.activities.filter((activity) => (
    (activity.kind === 'recording'
      ? ['saving', 'failed'].includes(activity.status)
      : ['starting', 'queued', 'transcribing', 'paused', 'saving', 'failed'].includes(activity.status))
    || (activity.status === 'ready' && Boolean(activity.warning))
  )).slice(0, 3)
  const showRecording = recordingState.isRecording

  if (!showRecording && visibleActivities.length === 0) return null
  return (
    <aside
      aria-label="Meeting activity"
      className="fixed bottom-4 right-4 z-30 w-[min(360px,calc(100vw-2rem))] space-y-2"
    >
      {showRecording && (
        <div className="rounded-xl border border-recording/30 bg-surface-raised p-3 shadow-lg">
          <div className="flex items-center gap-2">
            <span className={`h-2.5 w-2.5 rounded-full ${recordingState.isPaused ? 'bg-paused' : 'animate-pulse bg-recording'}`} />
            <span className="text-sm font-semibold text-ink">{recordingState.isPaused ? 'Recording paused' : 'Recording in progress'}</span>
            <div className="ml-auto flex gap-2">
              {controller.activeMeetingId && (
                <Button size="sm" variant="ghost" disabled={controller.isCommandPending} onClick={() => void controller.returnToRecording().catch(() => {})}>
                  Return
                </Button>
              )}
              <Button size="sm" variant="recording" disabled={controller.isCommandPending} onClick={() => void controller.stopRecording().catch(() => {})}>
                <Square className="h-3 w-3" /> Stop
              </Button>
            </div>
          </div>
          {!controller.activeMeetingId && (
            <StatusFeedback tone="warning" className="mt-2 text-xs">
              Capture is active while the meeting workspace is being prepared. Stop remains available here.
            </StatusFeedback>
          )}
        </div>
      )}
      {visibleActivities.map((activity) => (
        <div key={activity.task_id} className="rounded-xl border border-hairline bg-surface-raised p-3 shadow-lg">
          <p className="mb-1 truncate text-xs font-semibold text-ink">{activity.title}</p>
          <MeetingActivityFeedback activity={activity} />
          {activity.progress_percentage !== null && (
            <div className="mt-2 h-1.5 overflow-hidden rounded-full bg-surface-2" aria-label={`${activity.progress_percentage}% complete`}>
              <div className="h-full bg-info" style={{ width: `${Math.max(0, Math.min(100, activity.progress_percentage))}%` }} />
            </div>
          )}
        </div>
      ))}
    </aside>
  )
}

function ReadyApplication({ children }: { children: React.ReactNode }) {
  const [showImportDialog, setShowImportDialog] = useState(false)
  const [importFilePath, setImportFilePath] = useState<string | null>(null)
  const handleOpenImport = useCallback((filePath?: string | null) => {
    setImportFilePath(filePath ?? null)
    setShowImportDialog(true)
  }, [])
  const handleImportOpenChange = useCallback((open: boolean) => {
    setShowImportDialog(open)
    if (!open) setImportFilePath(null)
  }, [])
  const handleFileDrop = useCallback((paths: string[]) => {
    const audioFile = paths.find((path) => {
      const extension = path.split('.').pop()?.toLowerCase()
      return Boolean(extension && isAudioExtension(extension))
    })
    if (audioFile) handleOpenImport(audioFile)
    else if (paths.length > 0) toast.error('Please drop an audio file', { description: `Supported formats: ${getAudioFormatsDisplayList()}` })
  }, [handleOpenImport])

  return (
    <RecordingControllerProvider>
      <ImportDialogProvider onOpen={handleOpenImport}>
        <ShellProvider>
          <DownloadProgressToastProvider />
          <TranscriptionProgressToastProvider />
          <AutoSummaryProvider />
          <MeetingDetectedPrompt />
          <div className="flex min-h-screen bg-surface-0">
            <SimpleSidebar />
            <MainContent>{children}</MainContent>
          </div>
          <GlobalActivitySurface />
          <RecordingControllerFeedback />
          <TranscriptRecoveryMount />
          <CommandPalette />
          <ImportSurfaces open={showImportDialog} onOpenChange={handleImportOpenChange} filePath={importFilePath} onFileDrop={handleFileDrop} />
        </ShellProvider>
      </ImportDialogProvider>
    </RecordingControllerProvider>
  )
}

function ReadyProviderTree({ children }: { children: React.ReactNode }) {
  return (
    <MeetingActivityProvider>
      <RecordingStateProvider>
        <TranscriptProvider>
          <ConfigProvider>
            <OllamaDownloadProvider>
              <OnboardingProvider>
                <UpdateCheckProvider>
                  <SidebarProvider>
                    <TooltipProvider>
                      <ReadyApplication>{children}</ReadyApplication>
                    </TooltipProvider>
                  </SidebarProvider>
                </UpdateCheckProvider>
              </OnboardingProvider>
            </OllamaDownloadProvider>
          </ConfigProvider>
        </TranscriptProvider>
      </RecordingStateProvider>
    </MeetingActivityProvider>
  )
}

export function MainAppLayout({ children }: { children: React.ReactNode }) {
  const [startupState, setStartupState] = useState<StartupState>('checking')
  const [startupAttempt, setStartupAttempt] = useState(0)

  useEffect(() => {
    let cancelled = false
    setStartupState('checking')
    void invoke<{ completed: boolean } | null>('get_onboarding_status')
      .then((status) => {
        if (!cancelled) setStartupState(status?.completed ? 'ready' : 'setup-required')
      })
      .catch((error) => {
        console.error('[Layout] Failed to check onboarding status:', error)
        if (!cancelled) setStartupState('error')
      })
    return () => { cancelled = true }
  }, [startupAttempt])

  useEffect(() => {
    if (process.env.NODE_ENV !== 'production') return
    const handleContextMenu = (event: MouseEvent) => event.preventDefault()
    document.addEventListener('contextmenu', handleContextMenu)
    return () => document.removeEventListener('contextmenu', handleContextMenu)
  }, [])

  return (
    <html lang="en" className="dark" suppressHydrationWarning>
      <body className={`${sourceSans3.variable} ${sourceSerif4.variable} bg-surface-0 font-sans text-ink antialiased`}>
        <ThemeBootstrap />
        <AnalyticsProvider>
          {startupState === 'setup-required' ? (
            <OnboardingProvider>
              <OnboardingFlow onComplete={() => {
                setStartupState('checking')
                window.location.reload()
              }} />
            </OnboardingProvider>
          ) : startupState === 'ready' ? (
            <ReadyProviderTree>{children}</ReadyProviderTree>
          ) : (
            <BootSurface state={startupState} onRetry={() => setStartupAttempt((attempt) => attempt + 1)} />
          )}
          <ThemedToaster />
        </AnalyticsProvider>
      </body>
    </html>
  )
}

export default function RootLayout({ children }: { children: React.ReactNode }) {
  const pathname = usePathname()
  if (pathname === '/meeting-prompt') {
    return (
      <html lang="en" className="dark bg-transparent" suppressHydrationWarning>
        <body className={`${sourceSans3.variable} bg-transparent font-sans antialiased`}>{children}</body>
      </html>
    )
  }
  return <MainAppLayout>{children}</MainAppLayout>
}
