import { motion } from 'framer-motion';
import { Download, RefreshCw, Trash2 } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { cn } from '@/lib/utils';
import { formatFileSize } from '@/lib/whisper';

/** The status shape both the Whisper and Parakeet engines report. */
export type TranscriptModelStatus =
  | 'Available'
  | 'Missing'
  | { Downloading: { progress: number } }
  | { Error: string }
  | { Corrupted: { file_size: number; expected_min_size: number } };

interface TranscriptModelCardProps {
  displayName: string;
  /** One line under the name, e.g. what the model is good at and its size. */
  details: string;
  status: TranscriptModelStatus;
  sizeMb: number;
  isSelected: boolean;
  isRecommended: boolean;
  isCancelling: boolean;
  onSelect: () => void;
  onDownload: () => void;
  onCancel: () => void;
  onDelete: () => void;
}

/**
 * A transcription model in the settings list, styled like the summary model
 * cards (BuiltInModelManager) so both halves of model settings read as one.
 */
export function TranscriptModelCard({
  displayName,
  details,
  status,
  sizeMb,
  isSelected,
  isRecommended,
  isCancelling,
  onSelect,
  onDownload,
  onCancel,
  onDelete,
}: TranscriptModelCardProps) {
  const isAvailable = status === 'Available';
  const isMissing = status === 'Missing';
  const isError = typeof status === 'object' && 'Error' in status;
  const isCorrupted = typeof status === 'object' && 'Corrupted' in status;
  const downloadProgress =
    typeof status === 'object' && 'Downloading' in status ? status.Downloading.progress : null;
  const showProgress = downloadProgress !== null || isCancelling;
  const progress = downloadProgress ?? 0;

  return (
    <motion.div
      initial={{ opacity: 0, y: 5 }}
      animate={{ opacity: 1, y: 0 }}
      transition={{ duration: 0.2 }}
      className={cn(
        'rounded-lg border bg-card p-4 transition-colors',
        isSelected && isAvailable ? 'border-ink ring-2 ring-ink' : 'border-hairline',
        isAvailable && 'cursor-pointer'
      )}
      onClick={() => {
        if (isAvailable) onSelect();
      }}
    >
      <div className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
        <div className="min-w-0 flex-1">
          <div className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1">
            <span className="min-w-0 break-words text-base font-bold leading-snug text-ink">
              {displayName}
            </span>
            {isRecommended && (
              <span className="shrink-0 rounded bg-surface-2 px-2 py-0.5 text-xs font-medium text-ink-muted">
                Recommended
              </span>
            )}
            {isAvailable && !isCancelling && (
              <span className="flex shrink-0 items-center gap-1 text-xs font-medium text-success">
                <span className="h-2 w-2 rounded-full bg-success" />
                Ready
              </span>
            )}
            {isSelected && isAvailable && (
              <span className="shrink-0 rounded bg-selected px-2 py-0.5 text-xs font-medium text-selected-foreground">
                Selected
              </span>
            )}
            {isCorrupted && (
              <span className="shrink-0 rounded bg-error-subtle px-2 py-0.5 text-xs font-medium text-error">
                Corrupted
              </span>
            )}
            {isError && downloadProgress === null && (
              <span className="shrink-0 rounded bg-error-subtle px-2 py-0.5 text-xs font-medium text-error">
                Download failed
              </span>
            )}
          </div>
          <p className="mt-1 text-sm text-ink-muted">{details}</p>
        </div>

        {!isCancelling && (
          <div className="flex shrink-0 flex-wrap items-center gap-2 sm:ml-4 sm:justify-end">
            {isMissing && (
              <Button
                variant="outline"
                size="sm"
                className="min-w-[100px]"
                onClick={(e) => {
                  e.stopPropagation();
                  onDownload();
                }}
              >
                <Download className="h-4 w-4" />
                Download
              </Button>
            )}
            {isError && downloadProgress === null && (
              <Button
                variant="outline"
                size="sm"
                className="min-w-[100px]"
                onClick={(e) => {
                  e.stopPropagation();
                  onDownload();
                }}
              >
                <RefreshCw className="h-4 w-4" />
                Retry
              </Button>
            )}
            {isCorrupted && (
              <Button
                variant="outline"
                size="sm"
                onClick={(e) => {
                  e.stopPropagation();
                  onDownload();
                }}
              >
                <RefreshCw className="h-4 w-4" />
                Re-download
              </Button>
            )}
            {(isAvailable || isCorrupted) && (
              <button
                onClick={(e) => {
                  e.stopPropagation();
                  onDelete();
                }}
                className="rounded p-2 text-ink-muted transition-colors hover:bg-surface-2 hover:text-error"
                title="Delete model to free up space"
              >
                <Trash2 className="h-4 w-4" />
              </button>
            )}
          </div>
        )}
      </div>

      {showProgress && (
        <div className="mt-3 border-t border-hairline pt-3">
          <div className="mb-1 flex items-center justify-between">
            <span className="text-sm font-medium text-ink">
              {isCancelling ? 'Cancelling…' : 'Downloading…'}
            </span>
            {isCancelling ? (
              <span className="text-xs text-ink-muted">Cancellation requested</span>
            ) : (
              <div className="flex items-center gap-2">
                <span className="text-sm font-semibold text-ink">{Math.round(progress)}%</span>
                <Button
                  variant="ghost"
                  size="sm"
                  onClick={(e) => {
                    e.stopPropagation();
                    onCancel();
                  }}
                >
                  Cancel
                </Button>
              </div>
            )}
          </div>
          <div className="h-2 w-full overflow-hidden rounded-full bg-hairline">
            <motion.div
              className="h-full rounded-full bg-ink"
              initial={{ width: 0 }}
              animate={{ width: `${progress}%` }}
              transition={{ duration: 0.3, ease: 'easeOut' }}
            />
          </div>
          {sizeMb > 0 && (
            <p className="mt-1 text-xs text-ink-muted">
              {formatFileSize(Math.round((sizeMb * progress) / 100))} / {formatFileSize(sizeMb)}
            </p>
          )}
        </div>
      )}
    </motion.div>
  );
}
