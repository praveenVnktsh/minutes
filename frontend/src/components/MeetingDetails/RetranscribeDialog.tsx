import React, { useState, useEffect } from 'react';
import { RefreshCw, Languages } from 'lucide-react';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '../ui/dialog';
import { Button } from '../ui/button';
import { invoke } from '@tauri-apps/api/core';
import { toast } from 'sonner';
import { useTranscriptAvailability } from '@/lib/speechModels';
import { TranscriptionUnsupportedNotice } from '@/components/TranscriptionUnsupportedNotice';
import { cn } from '@/lib/utils';
import { panel } from '@/lib/theme-classes';

interface RetranscribeDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  meetingId: string;
  meetingFolderPath: string | null;
}

export function RetranscribeDialog({
  open,
  onOpenChange,
  meetingId,
  meetingFolderPath,
}: RetranscribeDialogProps) {
  // Only guards the in-flight invoke against double submission; it is not a progress state.
  const [isStarting, setIsStarting] = useState(false);
  const availability = useTranscriptAvailability(open);
  const transcriptBlocked = availability === 'missing' || availability === 'unsupported';

  useEffect(() => {
    if (open) setIsStarting(false);
  }, [open]);

  const handleStartRetranscription = async () => {
    if (!meetingFolderPath) {
      toast.error('Meeting folder path not available');
      return;
    }

    setIsStarting(true);

    try {
      // One model, English only: the backend ignores language/model/provider.
      await invoke('start_retranscription_command', {
        meetingId,
        meetingFolderPath,
        language: null,
        model: null,
        provider: null,
      });

      // The run now reports itself through the workspace's single progress surface.
      onOpenChange(false);
    } catch (err: any) {
      const errorMsg = typeof err === 'string' ? err : (err?.message || String(err));
      toast.error(errorMsg);
    } finally {
      setIsStarting(false);
    }
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-[450px]">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <RefreshCw className="h-5 w-5 text-info" />
            Enhance transcript
          </DialogTitle>
          <DialogDescription>
            Transcribe the recording again, replacing the current transcript.
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-3 py-4">
          <p className="flex items-center gap-2 text-sm text-ink-muted">
            <Languages className="h-4 w-4" />
            Transcribed with Parakeet (English)
          </p>
          {availability === 'unsupported' && <TranscriptionUnsupportedNotice />}
          {availability === 'missing' && (
            <div className={cn(panel.warning, 'rounded-lg p-3 text-sm')}>
              The transcript model is not downloaded yet. Download it in Settings → Transcription.
            </div>
          )}
        </div>

        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)}>
            Cancel
          </Button>
          <Button
            onClick={handleStartRetranscription}
            disabled={!meetingFolderPath || isStarting || transcriptBlocked}
          >
            <RefreshCw className="h-4 w-4 mr-2" />
            Start
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
