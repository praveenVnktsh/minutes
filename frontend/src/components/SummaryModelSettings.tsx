'use client';

import { ModelConfigForm } from '@/components/settings/ModelConfigForm';
import { SummaryLanguageSettings } from '@/components/SummaryLanguageSettings';
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
            <h3 className="mb-2 text-lg font-semibold text-ink">Automatic summaries</h3>
            <p id="auto-summary-description" className="text-sm text-ink-muted">
              Generate a summary after a meeting finishes processing.
            </p>
          </div>
          <Switch
            aria-label="Generate summaries automatically"
            aria-describedby="auto-summary-description"
            checked={isAutoSummary}
            onCheckedChange={toggleIsAutoSummary}
          />
        </div>
      </div>

      <SummaryLanguageSettings />

      <div className="rounded-lg border border-hairline bg-surface-raised p-6 shadow-sm">
        <ModelConfigForm />
      </div>
    </div>
  );
}
