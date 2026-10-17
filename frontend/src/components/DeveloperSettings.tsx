'use client';

import { toast } from 'sonner';
import { Switch } from '@/components/ui/switch';
import { setReplaysEnabled, useReplaysEnabled } from '@/lib/developerSettings';

/** Settings → Developer: tools for working on Minutes itself. */
export function DeveloperSettings() {
  const replays = useReplaysEnabled();
  const change = (enabled: boolean) => {
    setReplaysEnabled(enabled).catch((error) =>
      toast.error('Could not change the Replays setting', {
        description: error instanceof Error ? error.message : String(error),
      }),
    );
  };
  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between rounded-lg border border-hairline p-4">
        <div className="flex-1 pr-4">
          <div className="font-medium">Replays</div>
          <div className="text-sm text-ink-muted">
            Adds a Replays page to the sidebar that plays back pipeline replays (the runs in this
            Mac&apos;s app data <span className="font-mono">replays</span> folder) through the real
            recording screen, with a timeline, the meeting audio and a live Copilot. Runs stay on
            this machine. Live Copilot asks wait while a meeting is being recorded.
          </div>
        </div>
        <Switch checked={replays} onCheckedChange={change} aria-label="Replays" />
      </div>
    </div>
  );
}
