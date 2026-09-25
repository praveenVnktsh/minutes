import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Merge, Save } from 'lucide-react';
import { toast } from 'sonner';
import { Button } from '../ui/button';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from '../ui/dialog';

export interface SpeakerIdentity {
  speaker_id: string;
  display_name: string;
  segment_count: number;
  samples?: string[];
}

interface SpeakerCorrectionDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  meetingId: string;
  speakers: SpeakerIdentity[];
  onChanged: () => Promise<void>;
  /** Optional in-place rename handler (avoids a transcript refetch). */
  onRenamed?: (speakerId: string, displayName: string) => Promise<void>;
}

export function SpeakerCorrectionDialog({
  open,
  onOpenChange,
  meetingId,
  speakers,
  onChanged,
  onRenamed,
}: SpeakerCorrectionDialogProps) {
  const [names, setNames] = useState<Record<string, string>>({});
  const [mergeTargets, setMergeTargets] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState<string | null>(null);

  useEffect(() => {
    setNames(Object.fromEntries(speakers.map((speaker) => [speaker.speaker_id, speaker.display_name])));
  }, [speakers]);

  const rename = async (speaker: SpeakerIdentity) => {
    const displayName = names[speaker.speaker_id]?.trim();
    if (!displayName) return;
    setBusy(`rename:${speaker.speaker_id}`);
    try {
      if (onRenamed) {
        await onRenamed(speaker.speaker_id, displayName);
      } else {
        await invoke('rename_speaker', {
          meetingId,
          speakerId: speaker.speaker_id,
          displayName,
        });
        await onChanged();
      }
      toast.success(`Renamed ${speaker.speaker_id} to ${displayName}`);
    } catch (error) {
      toast.error(`Could not rename speaker: ${String(error)}`);
    } finally {
      setBusy(null);
    }
  };

  const merge = async (speaker: SpeakerIdentity) => {
    const target = mergeTargets[speaker.speaker_id];
    if (!target) return;
    setBusy(`merge:${speaker.speaker_id}`);
    try {
      await invoke('merge_speakers', {
        meetingId,
        sourceSpeakerId: speaker.speaker_id,
        targetSpeakerId: target,
      });
      await onChanged();
      toast.success(`Merged ${speaker.display_name} into ${speakers.find((item) => item.speaker_id === target)?.display_name || target}`);
    } catch (error) {
      toast.error(`Could not merge speakers: ${String(error)}`);
    } finally {
      setBusy(null);
    }
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[80vh] overflow-y-auto sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle>Manage speakers</DialogTitle>
          <DialogDescription>
            Give speakers real names or merge duplicate identities. Use the speaker selector beside any transcript segment to fix a single attribution.
          </DialogDescription>
        </DialogHeader>

        {speakers.length === 0 ? (
          <div className="rounded-lg border border-dashed p-6 text-center text-sm text-ink-muted">
            Identify speakers first, then return here to name and correct them.
          </div>
        ) : (
          <div className="space-y-3">
            {speakers.map((speaker) => (
              <div key={speaker.speaker_id} className="rounded-lg border border-hairline p-3">
                <div className="mb-2 flex items-center justify-between gap-3">
                  <div>
                    <div className="font-mono text-xs text-ink-muted">{speaker.speaker_id}</div>
                    <div className="text-xs text-ink-subtle">{speaker.segment_count} segments</div>
                  </div>
                  <div className="flex flex-1 items-center gap-2">
                    <input
                      value={names[speaker.speaker_id] ?? speaker.display_name}
                      onChange={(event) => setNames((current) => ({ ...current, [speaker.speaker_id]: event.target.value }))}
                      className="h-9 min-w-0 flex-1 rounded-md border border-hairline px-3 text-sm focus:border-focus focus:outline-none"
                      aria-label={`Name for ${speaker.speaker_id}`}
                    />
                    <Button
                      size="sm"
                      variant="outline"
                      onClick={() => rename(speaker)}
                      disabled={busy !== null || names[speaker.speaker_id]?.trim() === speaker.display_name}
                    >
                      <Save size={15} className="mr-1" /> Save
                    </Button>
                  </div>
                </div>
                {speaker.samples && speaker.samples.length > 0 && (
                  <div className="mb-2 space-y-1 border-t border-hairline pt-2">
                    {speaker.samples.map((sample, index) => (
                      <p key={index} className="line-clamp-2 text-xs italic text-ink-subtle">
                        “{sample}”
                      </p>
                    ))}
                  </div>
                )}
                {speakers.length > 1 && (
                  <div className="flex items-center gap-2 border-t border-hairline pt-2">
                    <span className="text-xs text-ink-muted">Merge into</span>
                    <select
                      value={mergeTargets[speaker.speaker_id] || ''}
                      onChange={(event) => setMergeTargets((current) => ({ ...current, [speaker.speaker_id]: event.target.value }))}
                      className="h-8 min-w-0 flex-1 rounded-md border border-hairline px-2 text-sm"
                    >
                      <option value="">Choose a speaker…</option>
                      {speakers.filter((item) => item.speaker_id !== speaker.speaker_id).map((item) => (
                        <option key={item.speaker_id} value={item.speaker_id}>{item.display_name}</option>
                      ))}
                    </select>
                    <Button
                      size="sm"
                      variant="outline"
                      onClick={() => merge(speaker)}
                      disabled={busy !== null || !mergeTargets[speaker.speaker_id]}
                    >
                      <Merge size={15} className="mr-1" /> Merge
                    </Button>
                  </div>
                )}
              </div>
            ))}
          </div>
        )}
      </DialogContent>
    </Dialog>
  );
}
