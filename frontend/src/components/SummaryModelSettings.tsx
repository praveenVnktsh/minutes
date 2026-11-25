'use client';

import { ModelConfigForm } from '@/components/settings/ModelConfigForm';
import { LiveSummarySettings } from '@/components/LiveSummarySettings';
import { LiveNotesModelSettings } from '@/components/LiveNotesModelSettings';
import { CopilotSettings } from '@/components/CopilotSettings';
import { SummaryLanguageSettings } from '@/components/SummaryLanguageSettings';
import { SummaryPromptSettings } from '@/components/SummaryPromptSettings';
import { Switch } from '@/components/ui/switch';
import { useConfig } from '@/contexts/ConfigContext';

interface SummaryModelSettingsProps {
  refetchTrigger?: number;
}

export function SummaryModelSettings({ refetchTrigger: _refetchTrigger }: SummaryModelSettingsProps) {
  const { isAutoSummary, toggleIsAutoSummary } = useConfig();

  return (
    <div className="flex flex-col gap-4">
      <div className="rounded-lg border border-hairline bg-surface-raised p-6 shadow-sm">
        <div className="flex items-center justify-between gap-4">
          <div>
            <h3 className="mb-2 text-lg font-semibold text-ink">Write AI notes when the meeting ends</h3>
            <p id="auto-summary-description" className="text-sm text-ink-muted">
              Once the meeting&apos;s transcript is ready, finish its AI notes (with the wrap-up and action items)
              and give the meeting a title. When off, the notes stay as they were written during the meeting.
            </p>
          </div>
          <Switch
            aria-label="Write AI notes when the meeting ends"
            aria-describedby="auto-summary-description"
            checked={isAutoSummary}
            onCheckedChange={toggleIsAutoSummary}
          />
        </div>
      </div>

      <LiveSummarySettings />

      <LiveNotesModelSettings />

      <CopilotSettings />

      <SummaryLanguageSettings />

      <div className="rounded-lg border border-hairline bg-surface-raised p-6 shadow-sm">
        <ModelConfigForm />
      </div>

      <SummaryPromptSettings kind="notes" />

      <SummaryPromptSettings kind="transcriptOnly" />
    </div>
  );
}
