import React from 'react';
import { Upload } from 'lucide-react';
import { getAudioFormatsDisplayList } from '@/constants/audioFormats';
import { cn } from '@/lib/utils';
import { scrim } from '@/lib/theme-classes';

interface ImportDropOverlayProps {
  visible: boolean;
}

export function ImportDropOverlay({ visible }: ImportDropOverlayProps) {
  if (!visible) return null;

  return (
    <div
      className={cn(
        'fixed inset-0 z-50 flex items-center justify-center pointer-events-none transition-opacity duration-200',
        scrim
      )}
    >
      <div className="border-2 border-dashed border-hairline rounded-2xl
                      p-12 text-center bg-surface-raised text-ink shadow-2xl
                      transform scale-100 transition-transform">
        <Upload className="h-16 w-16 text-ink-muted mx-auto mb-4" />
        <p className="text-xl font-medium">Drop audio file to import</p>
        <p className="text-sm text-ink-muted mt-2">{getAudioFormatsDisplayList()}</p>
      </div>
    </div>
  );
}
