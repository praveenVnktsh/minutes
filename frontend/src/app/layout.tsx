'use client'

import './globals.css'
import localFont from 'next/font/local'
import { useCallback, useEffect, useLayoutEffect, useState } from 'react'
import { usePathname } from 'next/navigation'
import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { AlertTriangle, LoaderCircle } from 'lucide-react'
import SimpleSidebar from '@/components/SimpleSidebar'
import { SidebarProvider } from '@/components/Sidebar/SidebarProvider'
import MainContent from '@/components/MainContent'
import AnalyticsProvider from '@/components/AnalyticsProvider'
import { TooltipProvider } from '@/components/ui/tooltip'
import { Button } from '@/components/ui/button'
import { StatusFeedback } from '@/components/ui/status-feedback'
import { MeetingActivityProvider } from '@/contexts/MeetingActivityContext'
import { RecordingStateProvider } from '@/contexts/RecordingStateContext'
import { OllamaDownloadProvider } from '@/contexts/OllamaDownloadContext'
import { TranscriptProvider } from '@/contexts/TranscriptContext'
import { ConfigProvider, useConfig } from '@/contexts/ConfigContext'
import { OnboardingProvider } from '@/contexts/OnboardingContext'
import { RecordingControllerProvider } from '@/contexts/RecordingControllerContext'
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
import { ShellActivitySurface } from '@/components/ShellActivitySurface'
import { SHARED_TOASTER_PROPS, ThemedToaster } from '@/components/ThemedToaster'
import { CommandPalette } from '@/components/CommandPalette'
import { isAudioExtension, getAudioFormatsDisplayList } from '@/constants/audioFormats'
import { applyTheme, listenForThemeChanges, readTheme, syncNativeTheme, type AppTheme } from '@/lib/theme'
import { settingsHref } from '@/components/settings/settingsSections'
import { Toaster, toast } from 'sonner'
import 'sonner/dist/styles.css'

// Self-hosted so the build never fetches from Google Fonts: next/font/google
// crashes the whole build when Google answers a CI runner with a font URL it
// cannot parse. Both files are the latin-subset variable fonts (wght 400-700),
// licensed under the OFL (see ./fonts/OFL-*.txt).
const sourceSans3 = localFont({
  src: './fonts/source-sans-3-latin-variable.woff2',
  weight: '400 700',
  variable: '--font-source-sans-3',
})

const sourceSerif4 = localFont({
  src: './fonts/source-serif-4-latin-variable.woff2',
  weight: '400 700',
  variable: '--font-source-serif-4',
})

export type StartupState = 'checking' | 'setup-required' | 'ready' | 'error'
const useBrowserLayoutEffect = typeof window === 'undefined' ? useEffect : useLayoutEffect

function ThemeBootstrap() {
  useBrowserLayoutEffect(() => {
    const theme = readTheme()
    applyTheme(theme)
    document.body.style.visibility = 'visible'
    void syncNativeTheme(theme)
  }, [])
  return null
}

function BootToaster() {
  const [theme, setTheme] = useState<AppTheme>(readTheme)
  useEffect(() => {
    let unlisten: UnlistenFn | undefined
    void listenForThemeChanges(setTheme).then((dispose) => { unlisten = dispose }).catch(() => {})
    return () => unlisten?.()
  }, [])
  return <Toaster theme={theme} {...SHARED_TOASTER_PROPS} />
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
      try {
        const unlisten = await registration
        if (disposed) unlisten()
        else unlisteners.push(unlisten)
      } catch (error) {
        if (!disposed) console.warn('[Import] Desktop drop events are unavailable:', error)
      }
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

function NavigationFeedback() {
  const { navigationError, retryNavigation } = useShell()
  if (!navigationError) return null
  return (
    <div className="fixed left-1/2 top-4 z-50 w-[min(520px,calc(100vw-2rem))] -translate-x-1/2 rounded-xl border border-error/30 bg-surface-raised p-3 shadow-lg">
      <StatusFeedback tone="error" actionLabel="Retry" onAction={() => void retryNavigation().catch(() => {})}>
        Notes could not be saved before navigation: {navigationError.message}
      </StatusFeedback>
    </div>
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
    <RecordingControllerProvider feedbackManagedGlobally>
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
          <ShellActivitySurface />
          <RecordingControllerFeedback />
          <NavigationFeedback />
          <TranscriptRecoveryMount />
          <CommandPalette />
          <ThemedToaster />
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
      <body style={{ visibility: 'hidden' }} className={`${sourceSans3.variable} ${sourceSerif4.variable} bg-surface-0 font-sans text-ink antialiased`}>
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
          {startupState !== 'ready' && <BootToaster />}
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
        <body style={{ visibility: 'hidden' }} className={`${sourceSans3.variable} bg-transparent font-sans antialiased`}>{children}</body>
      </html>
    )
  }
  return <MainAppLayout>{children}</MainAppLayout>
}
