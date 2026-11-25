import { cn } from '@/lib/utils';
import { panel } from '@/lib/theme-classes';
import { TRANSCRIPTION_OS_UNSUPPORTED_MESSAGE } from '@/lib/speechModels';

/** The status box shown in place of speech model downloads before macOS 15. */
export function TranscriptionUnsupportedNotice({ className }: { className?: string }) {
  return (
    <div role="status" className={cn(panel.warning, 'rounded-lg p-3 text-sm', className)}>
      {TRANSCRIPTION_OS_UNSUPPORTED_MESSAGE}
    </div>
  );
}
