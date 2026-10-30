import React, { useState, useEffect, useCallback, useRef } from 'react';
import {
  Upload,
  Languages,
  Loader2,
  AlertCircle,
  CheckCircle2,
  X,
  FileAudio,
  Clock,
  HardDrive,
} from 'lucide-react';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '../ui/dialog';
import { Button } from '../ui/button';
import { Input } from '../ui/input';
import { toast } from 'sonner';
import { useImportAudio, ImportResult } from '@/hooks/useImportAudio';
import { useRouter } from 'next/navigation';
import { useSidebar } from '../Sidebar/SidebarProvider';
import { useTranscriptAvailability } from '@/lib/speechModels';
import { TranscriptionUnsupportedNotice } from '@/components/TranscriptionUnsupportedNotice';
import { cn } from '@/lib/utils';
import { panel, progress as progressClasses, toneText } from '@/lib/theme-classes';


interface ImportAudioDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  preselectedFile?: string | null;
  onComplete?: () => void;
}

function formatDuration(seconds: number): string {
  const hours = Math.floor(seconds / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  const secs = Math.floor(seconds % 60);

  if (hours > 0) {
    return `${hours}:${minutes.toString().padStart(2, '0')}:${secs.toString().padStart(2, '0')}`;
  }
  return `${minutes}:${secs.toString().padStart(2, '0')}`;
}

function formatFileSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(1)} GB`;
}

export function ImportAudioDialog({
  open,
  onOpenChange,
  preselectedFile,
  onComplete,
}: ImportAudioDialogProps) {
  const router = useRouter();
  const { refetchMeetings } = useSidebar();
  const availability = useTranscriptAvailability(open);
  const transcriptBlocked = availability === 'missing' || availability === 'unsupported';

  const [title, setTitle] = useState('');
  const [titleModifiedByUser, setTitleModifiedByUser] = useState(false);

  // Always start as false — represents "dialog has not yet been opened".
  // Do NOT initialize from the `open` prop: if the component mounts with open=true
  // (e.g. drag-drop path), we still need the initialization effect to run.
  const prevOpenRef = useRef(false);

  const handleImportComplete = useCallback((result: ImportResult) => {
    toast.success(`Import complete! ${result.segments_count} segments created.`);

    // Refresh meetings list then navigate to the imported meeting
    refetchMeetings();
    onComplete?.();
    onOpenChange(false);
    router.push(`/meeting-details?id=${result.meeting_id}`);
  }, [router, refetchMeetings, onComplete, onOpenChange]);

  const handleImportError = useCallback((error: string) => {
    toast.error('Import failed', { description: error });
  }, []);

  const {
    status,
    fileInfo,
    progress,
    error,
    isProcessing,
    isBusy,
    selectFile,
    validateFile,
    startImport,
    cancelImport,
    reset,
  } = useImportAudio({
    onComplete: handleImportComplete,
    onError: handleImportError,
  });

  // Reset state only when dialog transitions from closed to open
  // This prevents re-initialization when config changes while dialog is already open (Bug #4 & #5)
  useEffect(() => {
    const wasOpen = prevOpenRef.current;
    prevOpenRef.current = open;

    // Only initialize when transitioning from closed (false) to open (true)
    if (open && !wasOpen) {
      reset();
      setTitle('');
      setTitleModifiedByUser(false);

      // Validate preselected file if provided
      if (preselectedFile) {
        validateFile(preselectedFile).then((info) => {
          if (info) {
            setTitle(info.filename);
          }
        });
      }
    }
  }, [open, preselectedFile, reset, validateFile]);

  // Update title when fileInfo changes
  useEffect(() => {
    if (fileInfo && !title && !titleModifiedByUser) {
      setTitle(fileInfo.filename);
    }
  }, [fileInfo, title, titleModifiedByUser]);

  const handleSelectFile = async () => {
    const info = await selectFile();
    if (info) {
      setTitle(info.filename);
    }
  };

  const handleStartImport = async () => {
    if (!fileInfo) return;

    // One model, English only: the backend ignores language/model/provider.
    await startImport(fileInfo.path, title || fileInfo.filename, null, null, null);
  };

  const handleCancel = async () => {
    if (isProcessing) {
      await cancelImport();
      toast.info('Import cancelled');
    }
    onOpenChange(false);
  };

  // Prevent closing during processing
  const handleOpenChange = (newOpen: boolean) => {
    if (!newOpen && isProcessing) {
      return;
    }
    onOpenChange(newOpen);
  };

  const handleEscapeKeyDown = (event: KeyboardEvent) => {
    if (isProcessing) {
      event.preventDefault();
    }
  };

  const handleInteractOutside = (event: Event) => {
    if (isProcessing) {
      event.preventDefault();
    }
  };

  return (
    <Dialog open={open} onOpenChange={handleOpenChange}>
      <DialogContent
        className="sm:max-w-[500px]"
        onEscapeKeyDown={handleEscapeKeyDown}
        onInteractOutside={handleInteractOutside}
      >
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            {isProcessing ? (
              <>
                <Loader2 className={cn('h-5 w-5 animate-spin', toneText.info)} />
                Importing Audio...
              </>
            ) : error ? (
              <>
                <AlertCircle className={cn('h-5 w-5', toneText.error)} />
                Import Failed
              </>
            ) : status === 'complete' ? (
              <>
                <CheckCircle2 className={cn('h-5 w-5', toneText.success)} />
                Import Complete
              </>
            ) : (
              <>
                <Upload className="h-5 w-5 text-ink-muted" />
                Import Audio File
              </>
            )}
          </DialogTitle>
          <DialogDescription>
            {isProcessing
              ? progress?.message || 'Processing audio...'
              : error
              ? 'An error occurred during import'
              : 'Import an audio file to create a new meeting with transcripts'}
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-4 py-4">
          {/* File selection / info */}
          {!isProcessing && !error && (
            <>
              {fileInfo ? (
                <div className="bg-surface-2 rounded-lg p-4 space-y-3">
                  <div className="flex items-start gap-3">
                    <FileAudio className="h-8 w-8 text-ink-muted flex-shrink-0" />
                    <div className="flex-1 min-w-0">
                      <p className="font-medium text-ink truncate">{fileInfo.filename}</p>
                      <div className="flex items-center gap-4 text-sm text-ink-muted mt-1">
                        <span className="flex items-center gap-1">
                          <Clock className="h-3.5 w-3.5" />
                          {formatDuration(fileInfo.duration_seconds)}
                        </span>
                        <span className="flex items-center gap-1">
                          <HardDrive className="h-3.5 w-3.5" />
                          {formatFileSize(fileInfo.size_bytes)}
                        </span>
                        <span className="font-medium text-ink">{fileInfo.format}</span>
                      </div>
                    </div>
                  </div>

                  {/* Editable title */}
                  <div className="space-y-1">
                    <label className="text-sm font-medium text-ink">Meeting Title</label>
                    <Input
                      value={title}
                      onChange={(e) => {
                        setTitle(e.target.value);
                        setTitleModifiedByUser(true);
                      }}
                      placeholder="Enter meeting title"
                    />
                  </div>

                  <Button variant="outline" size="sm" onClick={handleSelectFile} className="w-full">
                    Choose Different File
                  </Button>
                </div>
              ) : (
                <div className="border-2 border-dashed border-hairline rounded-lg p-8 text-center">
                  <FileAudio className="h-12 w-12 text-ink-subtle mx-auto mb-4" />
                  <Button onClick={handleSelectFile} disabled={status === 'validating'}>
                    {status === 'validating' ? (
                      <>
                        <Loader2 className="h-4 w-4 mr-2 animate-spin" />
                        Validating...
                      </>
                    ) : (
                      <>
                        <Upload className="h-4 w-4 mr-2" />
                        Select Audio File
                      </>
                    )}
                  </Button>
                  <p className="text-sm text-ink-muted mt-2">MP4, WAV, MP3, FLAC, OGG, MKV, WebM, WMA</p>
                </div>
              )}

              {fileInfo && (
                <p className="flex items-center gap-2 text-sm text-ink-muted">
                  <Languages className="h-4 w-4" />
                  Transcribed with Parakeet (English)
                </p>
              )}
              {availability === 'unsupported' && <TranscriptionUnsupportedNotice />}
              {availability === 'missing' && (
                <div className={cn(panel.warning, 'rounded-lg p-3 text-sm')}>
                  The transcript model is not downloaded yet. Download it in Settings → Transcription.
                </div>
              )}
            </>
          )}

          {/* Progress display */}
          {isProcessing && progress && (
            <div className="space-y-2">
              <div className="relative">
                <div className={cn('w-full rounded-full h-3', progressClasses.track)}>
                  <div
                    className={cn(progressClasses.fill, 'h-3 rounded-full transition-all duration-300 ease-out')}
                    style={{ width: `${Math.min(progress.progress_percentage, 100)}%` }}
                  />
                </div>
                <div className="flex justify-between text-xs text-ink-muted mt-1">
                  <span>{progress.stage}</span>
                  <span>{Math.round(progress.progress_percentage)}%</span>
                </div>
              </div>
              <p className="text-sm text-muted-foreground text-center">{progress.message}</p>
            </div>
          )}

          {/* Error display */}
          {error && (
            <div className={cn(panel.error, 'rounded-lg p-3')}>
              <p className="text-sm">{error}</p>
            </div>
          )}
        </div>

        <DialogFooter>
          {!isProcessing && !error && (
            <>
              <Button variant="outline" onClick={() => onOpenChange(false)}>
                Cancel
              </Button>
              <Button
                onClick={handleStartImport}
                disabled={!fileInfo || transcriptBlocked}
              >
                <Upload className="h-4 w-4 mr-2" />
                Import
              </Button>
            </>
          )}
          {isProcessing && (
            <Button variant="outline" onClick={handleCancel}>
              <X className="h-4 w-4 mr-2" />
              Cancel
            </Button>
          )}
          {error && (
            <>
              <Button variant="outline" onClick={() => onOpenChange(false)}>
                Close
              </Button>
              <Button onClick={reset} variant="outline">
                Try Again
              </Button>
            </>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
