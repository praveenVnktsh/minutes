'use client';

import { useCallback, useEffect, useState } from 'react';
import { toast } from 'sonner';
import { Label } from '@/components/ui/label';
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select';
import { Accordion, AccordionContent, AccordionItem, AccordionTrigger } from '@/components/ui/accordion';
import { finalPassApi, type FinalPassTiming } from '@/lib/liveTranscriptionModels';
import {
  LIVE_ENGINE_OPTIONS,
  liveTranscriptionApi,
  type LiveEngineChoice,
  type LiveTranscriptionSettings,
} from '@/lib/speechModels';

/** The engine to show as chosen. Anything the select does not offer (the
 * removed CPU engine, a newer value) reads as Auto, which is what Rust runs. */
export function liveEngineValue(settings: Pick<LiveTranscriptionSettings, 'liveEngine'> | null): LiveEngineChoice {
  const engine = settings?.liveEngine;
  return LIVE_ENGINE_OPTIONS.some((option) => option.value === engine) ? (engine as LiveEngineChoice) : 'auto';
}

function saveFailed(error: unknown) {
  toast.error('Could not save transcription settings', {
    description: error instanceof Error ? error.message : String(error),
  });
}

/** Where the live model runs, under an Advanced disclosure. */
export function LiveEngineSettings() {
  const [settings, setSettings] = useState<LiveTranscriptionSettings | null>(null);
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    liveTranscriptionApi.getSettings().then(setSettings).catch((error) => {
      console.error('Failed to load live transcription settings:', error);
    });
  }, []);

  const save = useCallback(async (liveEngine: LiveEngineChoice) => {
    const previous = settings;
    if (previous) setSettings({ ...previous, liveEngine });
    setSaving(true);
    try {
      setSettings(await liveTranscriptionApi.setSettings({ liveEngine }));
    } catch (error) {
      setSettings(previous);
      saveFailed(error);
    } finally {
      setSaving(false);
    }
  }, [settings]);

  return (
    <Accordion type="single" collapsible>
      <AccordionItem value="advanced" className="border-b-0">
        <AccordionTrigger className="py-2 text-sm font-medium text-ink-muted">Advanced</AccordionTrigger>
        <AccordionContent>
          <Label className="mb-1 block text-sm font-medium text-ink">Live transcription engine</Label>
          <p className="mx-1 mb-2 text-xs text-ink-muted">
            Where the live model runs. Auto uses the Neural Engine, which keeps the CPU and GPU
            free with the same accuracy.
          </p>
          <div className="mx-1">
            <Select
              value={liveEngineValue(settings)}
              disabled={!settings || saving}
              onValueChange={(value) => void save(value as LiveEngineChoice)}
            >
              <SelectTrigger aria-label="Live transcription engine" className="focus:ring-2 focus:ring-focus">
                <SelectValue placeholder="Auto" />
              </SelectTrigger>
              <SelectContent>
                {LIVE_ENGINE_OPTIONS.map((option) => (
                  <SelectItem key={option.value} value={option.value}>
                    {option.label}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
        </AccordionContent>
      </AccordionItem>
    </Accordion>
  );
}

/** Whether the transcript is redone after each meeting, and when. */
export function FinalPassTimingSettings() {
  const [timing, setTiming] = useState<FinalPassTiming | null>(null);
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    finalPassApi.getTiming().then(setTiming).catch((error) => {
      console.error('Failed to load when transcripts are improved:', error);
    });
  }, []);

  const save = useCallback(async (next: FinalPassTiming) => {
    const previous = timing;
    setTiming(next);
    setSaving(true);
    try {
      setTiming(await finalPassApi.setTiming(next));
    } catch (error) {
      setTiming(previous);
      saveFailed(error);
    } finally {
      setSaving(false);
    }
  }, [timing]);

  return (
    <div>
      <Label className="mb-1 block text-sm font-medium text-ink">Improve transcript after meetings</Label>
      <p className="mx-1 mb-2 text-xs text-ink-muted">
        Re-transcribes the recording with Parakeet and identifies speakers again. It is the
        heaviest work Minutes does, so by default it waits until your Mac is plugged in and out of Low Power Mode. Edited
        transcripts are never replaced.
      </p>
      <div className="mx-1">
        <Select
          value={timing ?? 'when-plugged-in'}
          disabled={timing === null || saving}
          onValueChange={(value) => void save(value as FinalPassTiming)}
        >
          <SelectTrigger aria-label="Improve transcript after meetings" className="focus:ring-2 focus:ring-focus">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="when-plugged-in">When plugged in (Recommended)</SelectItem>
            <SelectItem value="automatic">Automatically</SelectItem>
            <SelectItem value="off">Off</SelectItem>
          </SelectContent>
        </Select>
      </div>
    </div>
  );
}
