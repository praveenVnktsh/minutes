import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke as invokeTauri } from '@tauri-apps/api/core';
import { toast } from 'sonner';
import { errorMessage } from '@/lib/meetingCatalog';

interface UseTitleRegenerationProps {
  meetingId: string;
  onRegenerated?: (title: string) => void | Promise<void>;
}

export function useTitleRegeneration({
  meetingId,
  onRegenerated,
}: UseTitleRegenerationProps) {
  const [isRegenerating, setIsRegenerating] = useState(false);

  // Refs so the callback stays stable and can see the latest values after an await
  const inFlightRef = useRef(false);
  const meetingIdRef = useRef(meetingId);
  const onRegeneratedRef = useRef(onRegenerated);
  meetingIdRef.current = meetingId;
  onRegeneratedRef.current = onRegenerated;

  // A different meeting starts with a clean slate; a pending result for the old one is ignored
  useEffect(() => {
    inFlightRef.current = false;
    setIsRegenerating(false);
  }, [meetingId]);

  const regenerateTitle = useCallback(async () => {
    if (inFlightRef.current) return;
    inFlightRef.current = true;
    const requestedId = meetingId;
    setIsRegenerating(true);

    try {
      const title = await invokeTauri<string>('api_regenerate_meeting_title', {
        meetingId: requestedId,
      });
      if (meetingIdRef.current !== requestedId) return;
      await onRegeneratedRef.current?.(title);
    } catch (err) {
      if (meetingIdRef.current !== requestedId) return;
      console.error('Failed to regenerate meeting title:', err);
      toast.error('Failed to regenerate title', { description: errorMessage(err) });
    } finally {
      if (meetingIdRef.current === requestedId) {
        inFlightRef.current = false;
        setIsRegenerating(false);
      }
    }
  }, [meetingId]);

  return { regenerateTitle, isRegenerating };
}
